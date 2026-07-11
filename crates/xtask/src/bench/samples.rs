use std::{
    env, fs,
    io::BufReader,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use mirante4d_import::TiffImportSource;
use tiff::decoder::Decoder;

const DEFAULT_SAMPLE_IMPORT_FILE_LIMIT: usize = 4;

pub(crate) struct BenchmarkSampleSource {
    pub(crate) source: TiffImportSource,
    pub(crate) source_file_count: usize,
    pub(crate) selected_file_count: usize,
    pub(crate) file_limit: usize,
    pub(crate) selection_reason: String,
}

pub(crate) fn sample_import_file_limit() -> anyhow::Result<usize> {
    match env::var("MIRANTE4D_BENCH_IMPORT_MAX_FILES") {
        Ok(raw) => {
            let limit = raw
                .parse::<usize>()
                .with_context(|| format!("invalid MIRANTE4D_BENCH_IMPORT_MAX_FILES={raw:?}"))?;
            if limit == 0 {
                bail!("MIRANTE4D_BENCH_IMPORT_MAX_FILES must be greater than zero");
            }
            Ok(limit)
        }
        Err(env::VarError::NotPresent) => Ok(DEFAULT_SAMPLE_IMPORT_FILE_LIMIT),
        Err(err) => Err(err).context("failed to read MIRANTE4D_BENCH_IMPORT_MAX_FILES"),
    }
}

pub(crate) fn benchmark_sample_source(
    input_dir: &Path,
    output_root: &Path,
    dataset_id: &str,
    file_limit: usize,
) -> anyhow::Result<BenchmarkSampleSource> {
    let direct_tiffs = list_direct_tiff_files(input_dir)?;
    if let Some(preferred_raw) = preferred_single_file_sample(input_dir, &direct_tiffs) {
        return Ok(BenchmarkSampleSource {
            source: TiffImportSource::SingleFile(preferred_raw),
            source_file_count: direct_tiffs.len(),
            selected_file_count: 1,
            file_limit,
            selection_reason: "preferred single raw/intensity TIFF for untokened sample folder"
                .to_owned(),
        });
    }

    if !direct_tiffs.is_empty() {
        return benchmark_flat_stack_series_source(
            input_dir,
            &direct_tiffs,
            output_root,
            dataset_id,
            file_limit,
            "direct stack-series TIFF directory",
        );
    }

    let child_dirs = list_direct_child_dirs(input_dir)?;
    if child_dirs.is_empty() {
        bail!(
            "sample experiment contains no direct TIFF files or child source folders: {}",
            input_dir.display()
        );
    }

    if child_dirs.len() == 1 {
        let child_dir = &child_dirs[0];
        let child_tiffs = list_direct_tiff_files(child_dir)?;
        if let Some(preferred_raw) = preferred_single_file_sample(child_dir, &child_tiffs) {
            return Ok(BenchmarkSampleSource {
                source: TiffImportSource::SingleFile(preferred_raw),
                source_file_count: child_tiffs.len(),
                selected_file_count: 1,
                file_limit,
                selection_reason: "preferred single raw/intensity TIFF in child source folder"
                    .to_owned(),
            });
        }
        if !child_tiffs.is_empty() && tiff_has_multiple_images(&child_tiffs[0])? {
            return benchmark_flat_stack_series_source(
                child_dir,
                &child_tiffs,
                output_root,
                dataset_id,
                file_limit,
                "single child stack-series source folder",
            );
        }
    }

    let child_tiff_sets = child_dirs
        .iter()
        .map(|child_dir| {
            let tiffs = list_direct_tiff_files(child_dir)?;
            if tiffs.is_empty() {
                bail!(
                    "plane-series candidate channel folder contains no TIFF files: {}",
                    child_dir.display()
                );
            }
            Ok((child_dir.clone(), tiffs))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let source_file_count = child_tiff_sets
        .iter()
        .map(|(_, tiffs)| tiffs.len())
        .sum::<usize>();
    if source_file_count > file_limit {
        let subset_dir = output_root.join("input-subsets").join(format!(
            "{dataset_id}-{file_limit}-files-plane-series-volume"
        ));
        let selected_file_count =
            prepare_plane_series_subset(&child_tiff_sets, &subset_dir, file_limit)?;
        return Ok(BenchmarkSampleSource {
            source: TiffImportSource::Directory(subset_dir),
            source_file_count,
            selected_file_count,
            file_limit,
            selection_reason: "bounded folder-per-channel plane-series subset".to_owned(),
        });
    }

    Ok(BenchmarkSampleSource {
        source: TiffImportSource::Directory(input_dir.to_path_buf()),
        source_file_count,
        selected_file_count: source_file_count,
        file_limit,
        selection_reason: "complete folder-per-channel plane-series source".to_owned(),
    })
}

fn benchmark_flat_stack_series_source(
    source_dir: &Path,
    source_tiffs: &[PathBuf],
    output_root: &Path,
    dataset_id: &str,
    file_limit: usize,
    selection_reason: &str,
) -> anyhow::Result<BenchmarkSampleSource> {
    if source_tiffs.len() == 1 {
        return Ok(BenchmarkSampleSource {
            source: TiffImportSource::SingleFile(source_tiffs[0].clone()),
            source_file_count: source_tiffs.len(),
            selected_file_count: 1,
            file_limit,
            selection_reason: selection_reason.to_owned(),
        });
    }
    if source_tiffs.len() > file_limit {
        let subset_dir = output_root.join("input-subsets").join(format!(
            "{dataset_id}-{file_limit}-files-stack-series-movie"
        ));
        let selected_file_count = prepare_flat_tiff_subset(source_tiffs, &subset_dir, file_limit)?;
        return Ok(BenchmarkSampleSource {
            source: TiffImportSource::Directory(subset_dir),
            source_file_count: source_tiffs.len(),
            selected_file_count,
            file_limit,
            selection_reason: format!("bounded {selection_reason} subset"),
        });
    }
    Ok(BenchmarkSampleSource {
        source: TiffImportSource::Directory(source_dir.to_path_buf()),
        source_file_count: source_tiffs.len(),
        selected_file_count: source_tiffs.len(),
        file_limit,
        selection_reason: selection_reason.to_owned(),
    })
}

fn preferred_single_file_sample(input_dir: &Path, source_tiffs: &[PathBuf]) -> Option<PathBuf> {
    let preferred_names = ["volume_unnorm.tif", "raw_crop.tif", "crop2.tif"];
    preferred_names.iter().find_map(|name| {
        let candidate = input_dir.join(name);
        source_tiffs
            .iter()
            .any(|path| path == &candidate)
            .then_some(candidate)
    })
}

fn prepare_flat_tiff_subset(
    source_tiffs: &[PathBuf],
    subset_dir: &Path,
    file_limit: usize,
) -> anyhow::Result<usize> {
    if subset_dir.exists() {
        fs::remove_dir_all(subset_dir)
            .with_context(|| format!("failed to remove {}", subset_dir.display()))?;
    }
    fs::create_dir_all(subset_dir)
        .with_context(|| format!("failed to create {}", subset_dir.display()))?;
    let selected = file_limit.min(source_tiffs.len());
    for source in source_tiffs.iter().take(selected) {
        let file_name = source.file_name().context("TIFF path has no filename")?;
        link_or_copy_file(source, &subset_dir.join(file_name))?;
    }
    Ok(selected)
}

fn prepare_plane_series_subset(
    child_tiff_sets: &[(PathBuf, Vec<PathBuf>)],
    subset_dir: &Path,
    file_limit: usize,
) -> anyhow::Result<usize> {
    if subset_dir.exists() {
        fs::remove_dir_all(subset_dir)
            .with_context(|| format!("failed to remove {}", subset_dir.display()))?;
    }
    fs::create_dir_all(subset_dir)
        .with_context(|| format!("failed to create {}", subset_dir.display()))?;
    let channel_count = child_tiff_sets.len().max(1);
    let planes_per_channel = (file_limit / channel_count).max(1);
    let mut selected = 0usize;
    for (channel_dir, source_tiffs) in child_tiff_sets {
        let channel_name = channel_dir
            .file_name()
            .context("channel folder path has no filename")?;
        let destination_dir = subset_dir.join(channel_name);
        fs::create_dir_all(&destination_dir)
            .with_context(|| format!("failed to create {}", destination_dir.display()))?;
        for source in source_tiffs.iter().take(planes_per_channel) {
            let file_name = source.file_name().context("TIFF path has no filename")?;
            link_or_copy_file(source, &destination_dir.join(file_name))?;
            selected += 1;
        }
    }
    Ok(selected)
}

pub(crate) fn tiff_has_multiple_images(path: &Path) -> anyhow::Result<bool> {
    let file =
        fs::File::open(path).with_context(|| format!("failed to open TIFF {}", path.display()))?;
    let decoder = Decoder::new(BufReader::new(file))
        .with_context(|| format!("failed to decode TIFF {}", path.display()))?;
    Ok(decoder.more_images())
}

pub(crate) fn list_tiff_files(input_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let files = list_direct_tiff_files(input_dir)?;
    if files.is_empty() {
        bail!(
            "sample experiment contains no TIFF files: {}",
            input_dir.display()
        );
    }
    Ok(files)
}

pub(crate) fn list_direct_tiff_files(input_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(input_dir)
        .with_context(|| format!("failed to list {}", input_dir.display()))?
    {
        let entry = entry.with_context(|| format!("failed to list {}", input_dir.display()))?;
        let path = entry.path();
        if is_tiff_path(&path) {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

pub(crate) fn list_direct_child_dirs(input_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    for entry in fs::read_dir(input_dir)
        .with_context(|| format!("failed to list {}", input_dir.display()))?
    {
        let entry = entry.with_context(|| format!("failed to list {}", input_dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    Ok(dirs)
}

fn link_or_copy_file(source: &Path, destination: &Path) -> anyhow::Result<()> {
    fs::hard_link(source, destination)
        .or_else(|_| fs::copy(source, destination).map(|_| ()))
        .with_context(|| {
            format!(
                "failed to link or copy {} to {}",
                source.display(),
                destination.display()
            )
        })
}

fn is_tiff_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            extension.eq_ignore_ascii_case("tif") || extension.eq_ignore_ascii_case("tiff")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::audits::phase20::{write_phase20_u8_plane, write_phase20_u16_stack};
    use mirante4d_import::{TiffSourceProfile, inspect_tiff_source_for_review};

    #[test]
    fn phase20_benchmark_picker_subsets_plane_series_before_inspection() {
        let tempdir = tempfile::tempdir().unwrap();
        let sample = tempdir.path().join("T5-QUAL-001");
        let channel = sample.join("volume");
        let output_root = tempdir.path().join("out");
        fs::create_dir_all(&channel).unwrap();
        fs::create_dir_all(&output_root).unwrap();
        write_phase20_u8_plane(&channel.join("b.tif"), 20).unwrap();
        write_phase20_u8_plane(&channel.join("a.tif"), 10).unwrap();
        write_phase20_u8_plane(&channel.join("c.tif"), 30).unwrap();

        let picked = benchmark_sample_source(&sample, &output_root, "T5-QUAL-001", 2).unwrap();

        assert_eq!(picked.source_file_count, 3);
        assert_eq!(picked.selected_file_count, 2);
        assert_eq!(
            picked.selection_reason,
            "bounded folder-per-channel plane-series subset"
        );
        let source_path = picked.source.path();
        assert!(source_path.join("volume").join("a.tif").is_file());
        assert!(source_path.join("volume").join("b.tif").is_file());
        assert!(!source_path.join("volume").join("c.tif").exists());
        let inspection = inspect_tiff_source_for_review(&picked.source).unwrap();
        assert_eq!(
            inspection.source_profile,
            TiffSourceProfile::PlaneSeriesVolume
        );
        assert_eq!(inspection.channel_count, 1);
        assert_eq!(inspection.timepoint_count, 1);
        assert_eq!(inspection.shape.z, 2);
    }

    #[test]
    fn phase20_benchmark_picker_resolves_single_child_stack_series() {
        let tempdir = tempfile::tempdir().unwrap();
        let sample = tempdir.path().join("t5_qual_002");
        let stacks = sample.join("642_virus");
        let output_root = tempdir.path().join("out");
        fs::create_dir_all(&stacks).unwrap();
        fs::create_dir_all(&output_root).unwrap();
        for timepoint in 0..3 {
            write_phase20_u16_stack(
                &stacks.join(format!("timepoint-{timepoint:03}.tif")),
                timepoint,
            )
            .unwrap();
        }

        let picked = benchmark_sample_source(&sample, &output_root, "t5_qual_002", 2).unwrap();

        assert_eq!(picked.source_file_count, 3);
        assert_eq!(picked.selected_file_count, 2);
        assert_eq!(
            picked.selection_reason,
            "bounded single child stack-series source folder subset"
        );
        let source_path = picked.source.path();
        assert!(source_path.join("timepoint-000.tif").is_file());
        assert!(source_path.join("timepoint-001.tif").is_file());
        assert!(!source_path.join("timepoint-002.tif").exists());
        let inspection = inspect_tiff_source_for_review(&picked.source).unwrap();
        assert_eq!(
            inspection.source_profile,
            TiffSourceProfile::StackSeriesMovie
        );
        assert_eq!(inspection.timepoint_count, 2);
        assert_eq!(inspection.shape.z, 2);
    }

    #[test]
    fn tiff_path_detection_is_case_insensitive() {
        assert!(is_tiff_path(Path::new("a.TIF")));
        assert!(is_tiff_path(Path::new("a.tiff")));
        assert!(!is_tiff_path(Path::new("a.png")));
    }
}
