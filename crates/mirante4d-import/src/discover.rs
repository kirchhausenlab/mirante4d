use super::*;

pub(super) fn discover_tiffs(input_dir: &Path) -> Result<Vec<TiffInput>, ImportError> {
    if !input_dir.is_dir() {
        return Err(ImportError::MissingInputDirectory(input_dir.to_path_buf()));
    }
    let mut inputs = Vec::new();
    for entry in fs::read_dir(input_dir).map_err(|source| ImportError::ListInput {
        path: input_dir.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| ImportError::ListInput {
            path: input_dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if !is_tiff_path(&path) {
            continue;
        }
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let channel = parse_number_after_token(filename, "ch")
            .ok_or_else(|| ImportError::MissingChannel(path.clone()))?;
        let stack_index = parse_number_after_token(filename, "stack")
            .ok_or_else(|| ImportError::MissingStackIndex(path.clone()))?;
        inputs.push(TiffInput {
            path,
            channel,
            stack_index: u64::from(stack_index),
        });
    }
    if inputs.is_empty() {
        return Err(ImportError::EmptyInputDirectory(input_dir.to_path_buf()));
    }
    inputs.sort_by(|a, b| {
        a.channel
            .cmp(&b.channel)
            .then(a.stack_index.cmp(&b.stack_index))
            .then(a.path.cmp(&b.path))
    });
    Ok(inputs)
}

pub(super) fn discover_tiff_source(
    source: &TiffImportSource,
) -> Result<Vec<TiffInput>, ImportError> {
    match source {
        TiffImportSource::Directory(input_dir) => discover_tiffs(input_dir),
        TiffImportSource::SingleFile(input_file) => {
            if !input_file.is_file() {
                return Err(ImportError::MissingInputFile(input_file.clone()));
            }
            if !is_tiff_path(input_file) {
                return Err(ImportError::UnsupportedTiffPath(input_file.clone()));
            }
            Ok(vec![TiffInput {
                path: input_file.clone(),
                channel: 0,
                stack_index: 0,
            }])
        }
    }
}

pub(super) fn discover_tiff_source_for_import(
    source: &TiffImportSource,
    file_grouping: Option<&[TiffFileGrouping]>,
    source_profile: TiffSourceProfile,
) -> Result<Vec<TiffInput>, ImportError> {
    match file_grouping {
        Some(file_grouping) => tiff_inputs_from_explicit_grouping(source, file_grouping),
        None => {
            let discovered = discover_tiff_source_for_review(source)?;
            if discovered.source_profile != source_profile {
                return Err(ImportError::ReviewedSourceProfileMismatch {
                    reviewed: source_profile,
                    inspected: discovered.source_profile,
                });
            }
            Ok(discovered.inputs)
        }
    }
}

pub(super) fn discover_tiff_source_for_review(
    source: &TiffImportSource,
) -> Result<TiffInputSet, ImportError> {
    match source {
        TiffImportSource::Directory(input_dir) => discover_directory_source_for_review(input_dir),
        TiffImportSource::SingleFile(_) => Ok(TiffInputSet {
            source_profile: TiffSourceProfile::StackSeriesMovie,
            inputs: discover_tiff_source(source)?,
        }),
    }
}

pub(super) fn discover_directory_source_for_review(
    input_dir: &Path,
) -> Result<TiffInputSet, ImportError> {
    if !input_dir.is_dir() {
        return Err(ImportError::MissingInputDirectory(input_dir.to_path_buf()));
    }
    let mut tiff_paths = Vec::new();
    let mut child_dirs = Vec::new();
    for entry in fs::read_dir(input_dir).map_err(|source| ImportError::ListInput {
        path: input_dir.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| ImportError::ListInput {
            path: input_dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if is_tiff_path(&path) {
            tiff_paths.push(path);
        } else if path.is_dir() {
            child_dirs.push(path);
        }
    }
    if !tiff_paths.is_empty() && !child_dirs.is_empty() {
        return Err(ImportError::AmbiguousTiffSourceLayout {
            path: input_dir.to_path_buf(),
            message: "direct TIFF files are mixed with child folders".to_owned(),
        });
    }
    if !tiff_paths.is_empty() {
        return Ok(TiffInputSet {
            source_profile: TiffSourceProfile::StackSeriesMovie,
            inputs: tiff_inputs_from_stack_series_paths(tiff_paths),
        });
    }
    if !child_dirs.is_empty() {
        return Ok(TiffInputSet {
            source_profile: TiffSourceProfile::PlaneSeriesVolume,
            inputs: discover_plane_series_for_review(input_dir, child_dirs)?,
        });
    }
    Err(ImportError::EmptyInputDirectory(input_dir.to_path_buf()))
}

pub(super) fn tiff_inputs_from_stack_series_paths(mut paths: Vec<PathBuf>) -> Vec<TiffInput> {
    paths.sort();
    paths
        .into_iter()
        .enumerate()
        .map(|(index, path)| {
            let filename = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned();
            let channel = parse_number_after_token(&filename, "ch").unwrap_or(0);
            let stack_index = parse_number_after_token(&filename, "stack")
                .map(u64::from)
                .unwrap_or(index as u64);
            TiffInput {
                path,
                channel,
                stack_index,
            }
        })
        .collect()
}

pub(super) fn discover_plane_series_for_review(
    input_dir: &Path,
    mut child_dirs: Vec<PathBuf>,
) -> Result<Vec<TiffInput>, ImportError> {
    child_dirs.sort();
    let mut inputs = Vec::new();
    for (channel, channel_dir) in child_dirs.into_iter().enumerate() {
        let mut plane_paths = Vec::new();
        for entry in fs::read_dir(&channel_dir).map_err(|source| ImportError::ListInput {
            path: channel_dir.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| ImportError::ListInput {
                path: channel_dir.clone(),
                source,
            })?;
            let path = entry.path();
            if path.is_dir() {
                return Err(ImportError::InvalidPlaneSeriesLayout {
                    path,
                    message: "nested channel folders are not allowed".to_owned(),
                });
            }
            if is_tiff_path(&path) {
                plane_paths.push(path);
            }
        }
        if plane_paths.is_empty() {
            return Err(ImportError::InvalidPlaneSeriesLayout {
                path: channel_dir,
                message: "channel folder contains no TIFF plane files".to_owned(),
            });
        }
        plane_paths.sort();
        let channel =
            u32::try_from(channel).map_err(|err| ImportError::InvalidPlaneSeriesLayout {
                path: input_dir.to_path_buf(),
                message: format!("too many channel folders: {err}"),
            })?;
        inputs.extend(
            plane_paths
                .into_iter()
                .enumerate()
                .map(|(index, path)| TiffInput {
                    path,
                    channel,
                    stack_index: index as u64,
                }),
        );
    }
    if inputs.is_empty() {
        return Err(ImportError::EmptyInputDirectory(input_dir.to_path_buf()));
    }
    Ok(inputs)
}

pub(super) fn tiff_inputs_from_explicit_grouping(
    source: &TiffImportSource,
    file_grouping: &[TiffFileGrouping],
) -> Result<Vec<TiffInput>, ImportError> {
    if file_grouping.is_empty() {
        return Err(ImportError::EmptyFileGrouping);
    }
    let mut seen_paths = BTreeSet::new();
    let mut inputs = Vec::with_capacity(file_grouping.len());
    for grouping in file_grouping {
        validate_grouped_tiff_path(source, &grouping.path)?;
        if !seen_paths.insert(grouping.path.clone()) {
            return Err(ImportError::DuplicateFileGroupingPath(
                grouping.path.clone(),
            ));
        }
        inputs.push(TiffInput {
            path: grouping.path.clone(),
            channel: grouping.channel,
            stack_index: grouping.stack_index,
        });
    }
    inputs.sort_by(|a, b| {
        a.channel
            .cmp(&b.channel)
            .then(a.stack_index.cmp(&b.stack_index))
            .then(a.path.cmp(&b.path))
    });
    Ok(inputs)
}

pub(super) fn validate_grouped_tiff_path(
    source: &TiffImportSource,
    path: &Path,
) -> Result<(), ImportError> {
    if !path.is_file() {
        return Err(ImportError::MissingInputFile(path.to_path_buf()));
    }
    if !is_tiff_path(path) {
        return Err(ImportError::UnsupportedTiffPath(path.to_path_buf()));
    }
    match source {
        TiffImportSource::Directory(input_dir) => {
            let parent = path.parent().unwrap_or_else(|| Path::new(""));
            if parent != input_dir {
                return Err(ImportError::GroupedFileOutsideInputDirectory {
                    path: path.to_path_buf(),
                    input_dir: input_dir.to_path_buf(),
                });
            }
        }
        TiffImportSource::SingleFile(input_file) => {
            if path != input_file {
                return Err(ImportError::GroupedFileOutsideInputDirectory {
                    path: path.to_path_buf(),
                    input_dir: input_file.to_path_buf(),
                });
            }
        }
    }
    Ok(())
}

pub(super) fn group_by_channel(inputs: Vec<TiffInput>) -> BTreeMap<u32, Vec<TiffInput>> {
    let mut grouped: BTreeMap<u32, Vec<TiffInput>> = BTreeMap::new();
    for input in inputs {
        grouped.entry(input.channel).or_default().push(input);
    }
    grouped
}

pub(super) fn is_tiff_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            extension.eq_ignore_ascii_case("tif") || extension.eq_ignore_ascii_case("tiff")
        })
        .unwrap_or(false)
}

pub(super) fn parse_number_after_token(filename: &str, token: &str) -> Option<u32> {
    let start = filename.find(token)? + token.len();
    let digits = filename[start..]
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect::<String>();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

pub(super) fn channel_color(channel: u32) -> [f32; 4] {
    match channel {
        0 => [0.0, 1.0, 0.0, 1.0],
        1 => [1.0, 0.0, 1.0, 1.0],
        2 => [0.0, 0.6, 1.0, 1.0],
        _ => [1.0, 1.0, 1.0, 1.0],
    }
}
