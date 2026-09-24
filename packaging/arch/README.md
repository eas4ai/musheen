# Arch Linux package

From the repository root, run:

```sh
MUSHEEN_SCRATCH_BASE=/path/to/scratchpads scripts/build-arch-package.sh
```

The script uses one local Docker build. It compiles all features with eight
Cargo jobs and incremental builds disabled, runs the Rust tests, checks the
desktop metadata, builds a pacman package, then tests install and removal in a
fresh Arch image. It prints the package path under a unique scratch directory.
Set `DOCKER_CONFIG` to your existing Docker configuration directory if needed.

On an Arch Linux host, install the resulting `musheen-*.pkg.tar.zst` with
`pacman -U`. Do not install this package on a different distribution.
