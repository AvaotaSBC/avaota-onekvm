# Sun55i Cedar encoder

Build the ARM64 binary and package:

```sh
cross build --release --target aarch64-unknown-linux-gnu --features rkmpp
ONE_KVM_SUN55I_CEDAR=1 ./build/package-deb.sh arm64
```

The `one-kvm-sun55i` package depends on `avaota-cedar-encoder`. One-KVM lists
`Cedar (Sun55i)` for H.264 when the Cedar bridge and `/dev/cedar_dev` are present.
`Auto` selects Cedar for H.264 and retains the available encoders for other formats.

Push a `sun55i-v<package-version>` tag to build an ARM64 deb and attach it to a
GitHub Release. For example: `sun55i-v0.2.9+sun55i20261124.8`.
