use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use glam::{DQuat, DVec3};
use mirante4d_core::{
    CameraView, DEFAULT_PRESENTATION_VIEWPORT_POINTS, GridToWorld, Projection, Shape4D,
};

fn bench_shape_chunk_grid(criterion: &mut Criterion) {
    let shape = Shape4D::new(8, 512, 512, 512).unwrap();
    let chunk_shape = Shape4D::new(1, 64, 64, 64).unwrap();

    criterion.bench_function("shape4d_chunk_grid", |bencher| {
        bencher.iter(|| black_box(shape).chunk_grid(black_box(chunk_shape)).unwrap())
    });
}

fn bench_grid_world_round_trip(criterion: &mut Criterion) {
    let grid_to_world = GridToWorld::scale_um(0.108, 0.108, 0.35);
    let world_to_grid = grid_to_world.inverse().unwrap();
    let point = DVec3::new(127.5, 243.0, 31.25);

    criterion.bench_function("grid_world_round_trip", |bencher| {
        bencher.iter(|| {
            let world = black_box(grid_to_world).transform_point(black_box(point));
            black_box(world_to_grid).transform_point(black_box(world))
        })
    });
}

fn bench_orthographic_ray_generation(criterion: &mut Criterion) {
    let camera = CameraView::new(
        Projection::Orthographic,
        DVec3::new(256.0, 256.0, 64.0),
        (DQuat::from_rotation_y(-0.35) * DQuat::from_rotation_x(-0.15)).normalize(),
        1.0,
        320.0,
        80.0,
    )
    .to_camera_state(DEFAULT_PRESENTATION_VIEWPORT_POINTS);

    criterion.bench_function("orthographic_ray_generation", |bencher| {
        bencher.iter(|| black_box(camera).ray_for_screen_point(black_box(37.0), black_box(-42.0)))
    });
}

criterion_group!(
    core_primitives,
    bench_shape_chunk_grid,
    bench_grid_world_round_trip,
    bench_orthographic_ray_generation
);
criterion_main!(core_primitives);
