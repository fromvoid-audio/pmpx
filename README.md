# pmpx

pmpx is a gain shaper for drawing rhythmic volume curves or capturing them from live sidechain compression. It builds as CLAP by default and supports VST3 with the Steinberg SDK.

- Draw a Bézier envelope and sync it to your DAW or run it at a fixed rate.
- Shape the full signal or split it into two bands with a movable crossover.
- Capture one cycle of compression as an editable curve, then save it to one of eight shared shape slots.

## Build

Requires Rust. Building VST3 also requires the [Steinberg VST3 SDK](https://github.com/steinbergmedia/vst3sdk) (`VST3_SDK_DIR`).

```sh
cargo build --release --locked                                               # CLAP (default)
VST3_SDK_DIR=/path/to/vst3sdk cargo build --release --locked --features vst3 # VST3
VST3_SDK_DIR=/path/to/vst3sdk bash scripts/ci_local.sh --vst3                # local checks (VST3)
```

pmpx was previously called Pump. Existing plug-in IDs, project state, and presets carry forward.

## About

I’m [PORTALSURFER](https://soundcloud.com/portalsurfer). I make music and build instruments and tools for making it under [fromvoid](https://github.com/fromvoid-audio). I use AI extensively in that work and share these projects so more people can make music, whatever their budget.

I build this software for myself first and foremost and offer no guarantees of support. That said, I welcome code, documentation, and interesting ideas, and I’m happy to add them when we align on the direction.

---

Copyright wasn't just inconvenienced by AI; perhaps it was always the wrong bet.

We treated ideas like property, then acted surprised when progress slowed and wealth pooled upward. But evolution has always followed a simple pattern: copying, mutation, and selection. Ideas, like living things, develop through variation and iteration.

Perhaps the mistake was never copying, but our insistence on preventing it.

Soon, software will be something we simply wish into existence, endlessly adapting to our needs. When creation becomes abundant, artificial scarcity loses its purpose.

fromvoid is built in the open, not as charity, but as a refusal to restrict the evolution of ideas.

Copy. Mutate. Select. Repeat.

## Support

If you’d like to help cover development costs, you can chip in on Ko-fi:

<a href='https://ko-fi.com/F7X2225YCF' target='_blank'><img height='36' style='border:0px;height:36px;' src='https://storage.ko-fi.com/cdn/kofi6.png?v=6' border='0' alt='Buy Me a Coffee at ko-fi.com' /></a>

---

[User guide](docs/USER-GUIDE.md) · [Development and releases](docs/README.md) · [MIT license](LICENSE)
