pub(crate) const MIP_SHADER: &str = r#"
const SAMPLE_INVALID_FLAG: u32 = 0x00010000u;
const OUTPUT_COVERED_FLAG: u32 = 0x80000000u;

struct Params {
    width: u32,
    height: u32,
    depth: u32,
    _pad: u32,
};

@group(0) @binding(0)
var<storage, read> voxels: array<u32>;

@group(0) @binding(1)
var<storage, read_write> output_pixels: array<u32>;

@group(0) @binding(2)
var<uniform> params: Params;

fn sample_value(sample: u32) -> u32 {
    return sample & 0xffffu;
}

fn sample_covered(sample: u32) -> bool {
    return (sample & SAMPLE_INVALID_FLAG) == 0u;
}

fn pack_output(value: u32, covered: bool) -> u32 {
    return (value & 0xffffu) | select(0u, OUTPUT_COVERED_FLAG, covered);
}

@compute @workgroup_size(8, 8, 1)
fn mip_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    let y = id.y;
    if (x >= params.width || y >= params.height) {
        return;
    }

    var max_value = 0u;
    var covered = false;
    var z = 0u;
    loop {
        if (z >= params.depth) {
            break;
        }
        let index = (z * params.height + y) * params.width + x;
        let sample = voxels[index];
        if (sample_covered(sample)) {
            max_value = max(max_value, sample_value(sample));
            covered = true;
        }
        z = z + 1u;
    }

    output_pixels[y * params.width + x] = pack_output(max_value, covered);
}
"#;
