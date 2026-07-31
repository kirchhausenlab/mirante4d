// Shared premultiplied front-to-back composition for volume kernels.

fn composite_over(near: PixelResult, far: PixelResult) -> PixelResult {
    let remaining = 1.0 - near.alpha;
    return PixelResult(
        near.premultiplied_rgb + far.premultiplied_rgb * remaining,
        near.alpha + far.alpha * remaining,
        near.covered & far.covered,
        near.valid | far.valid,
        min(near.depth, far.depth),
    );
}
