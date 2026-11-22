# pmpx

<a href='https://ko-fi.com/F7X2225YCF' target='_blank'><img height='36' style='border:0px;height:36px;' src='https://storage.ko-fi.com/cdn/kofi6.png?v=6' border='0' alt='Buy Me a Coffee at ko-fi.com' /></a>

pmpx is a gain shaper for drawing rhythmic volume curves or capturing them from live sidechain compression. It builds as CLAP by default and supports VST3 with the Steinberg SDK.

- Draw a Bézier envelope and sync it to your DAW or run it at a fixed rate.
- Shape the full signal or split it into two bands with a movable crossover.
- Capture one cycle of compression as an editable curve, then save it to one of eight shared shape slots.

pmpx was previously called Pump. Existing plug-in IDs, project state and presets carry forward.

Run `cargo build` for CLAP. For VST3, run `VST3_SDK_DIR=/path/to/vst3sdk bash scripts/ci_local.sh --vst3`.

[User guide](docs/USER-GUIDE.md) · [Development and releases](docs/README.md) · [MIT license](LICENSE)
