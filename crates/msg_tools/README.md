# msg_convert

`msg-convert` converts AMReX plotfiles and VTK XML ImageData (`.vti`) files
into MSG archives. Formats are inferred from paths when possible.

```sh
msg-convert plt00010 plt00010.msg
msg-convert simulation.vti simulation.msg
msg-convert --config conversion.toml
```

Configuration is split by responsibility:

- `[input]` and `[output]` identify files and optional formats.
- `[source]` selects imported variables or arrays.
- `[source.amrex]` controls AMReX-specific invalid-value handling.
- `[encoding.defaults]` sets the default MSG codec policy.
- `[[encoding.fields]]` transforms or overrides individual final MSG fields.

```toml
[input]
path = "simulation.vti"

[output]
path = "simulation.msg"

[source]
fields = ["density", "velocity"]

[encoding.defaults]
codec = "auto:lossless"

[[encoding.fields]]
# Multi-component VTI arrays become final fields such as velocity_x.
name = "velocity_x"
codec = "quantized:u16"
error = { relative_percent = 0.1 }
```

AMReX source fields default to `f32`; use `precision = "f64"` when required.
The invalid-value policy is either `"reject"` or `"zero"`.

```toml
[input]
path = "plt00010"
format = "amrex"

[output]
path = "plt00010.msg"

[source]
fields = [
    "temperature",
    { name = "density", precision = "f64" },
]

[source.amrex]
invalid_values = "reject"

[[encoding.fields]]
name = "density"
normalize = { min = 0.0, max = 1.0 }
codec = "auto:smallest"
error = { absolute = 0.0, relative_percent = 0.01 }
```

Normalization maps and clamps a final field with
`(value - min) / (max - min)`. Stored values are in `[0, 1]`, while the
original finite, strictly increasing bounds remain in archive metadata. Codec
error tolerances remain expressed in the original physical units.

Command-line input and output paths may override both configured paths as a
pair. `--field` and `--amrex-replace-invalid` are available only without
`--config`.

## Examples

Annotated configurations are available in [`examples/`](examples):

- [`amrex.toml`](examples/amrex.toml) covers source precision, invalid values,
  normalization, and a per-field codec override.
- [`vti.toml`](examples/vti.toml) covers array selection, vector components,
  normalization, and error budgets.
- [`codecs.toml`](examples/codecs.toml) lists every codec and its requirements.

Run one after replacing its sample paths and field names:

```sh
msg-convert --config examples/amrex.toml
```

## Archive probe

`msg-probe` validates an MSG archive and reports geometry, level layout,
normalization and error metadata, decoded value ranges and sample counts,
per-codec chunk counts and storage, block-level storage, and dense-size ratios.

```sh
msg-probe simulation.msg
```

For a fast report that validates metadata and blocks without decoding scalar
chunks:

```sh
msg-probe --metadata-only simulation.msg
```
