# Glossary

## 4D

3D volume data over time.

## Brick

A spatial block of voxels used as the runtime unit for storage, cache, GPU upload, and traversal.

## Chunk

A storage unit in chunked array formats such as Zarr. In Mirante4D docs, "brick" is usually the renderer/runtime concept and "chunk" is usually the storage concept. They may map 1:1, but that should be an explicit format choice.

## Dense Intensity

Voxel data where most spatial positions have meaningful intensity values, commonly represented as regular bricked arrays.

## Hard Cutover

A development policy where old behavior is replaced outright instead of preserved through compatibility branches or fallback paths.

## Mirante4D Native Dataset

The strict dataset format accepted by the Mirante4D core app. Current format string: `mirante4d-v1`.

## OME-Zarr

A bioimaging metadata convention on top of Zarr, also known as OME-NGFF.

## Residency

The subset of dataset resources currently loaded into CPU memory or GPU memory.

## Shard

A larger storage object containing multiple chunks or payloads, used to avoid excessive small files and improve I/O behavior.

## Zarr v3

A chunked N-dimensional array storage format with modern metadata, codec pipelines, and sharding support.
