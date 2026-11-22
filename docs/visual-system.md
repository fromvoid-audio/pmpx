# Pump visual system

Pump follows Frame's Technical Futurist R7 audio-device rules at commit `5815ef6`. The native GPUI palette lives in `src/gui/visual_system.rs`; composition and provenance are documented in `ui-composition.md`.

| Role | Color |
| --- | --- |
| Chassis | `#2A2A2A` |
| Controls / overlays | `#353535` |
| Recessed display | `#232323` |
| Border | `#505050` |
| Emphasized bevel | `#646464` |
| Coral accent | `#E96B50` |
| Primary text | `#D7D7D7` |
| Secondary text | `#A9A9A9` |
| Warning / overload | `#D75C49` |
| Focus | `#8CDDD0` |
| LP / HP curves | `#76B89D` / `#809EC6` |
| LP / HP opaque slider fills | `#415F4F` / `#3F526A` |
| Version | `#515151` |

The historical `accent_mint` and `accent_copper` token names both map to coral. Band colors preserve the user's explicit mint/blue preference. Focus is distinguished with an outline; active buttons and sound sides also use geometry/text state. The meter uses an opaque continuous fill with overload color above 75% of its reduction range.

Ioskeley Mono is the primary typeface, with the existing fallback chain. Font size / line height: brand 17/24, body and values 11/14, control labels 9/12, metadata 8/10. The real version alone uses 6/8.

The spacing unit is 4 px, surface padding 16, standard gap 8, controls 24 high, icon targets 28, knobs 32, and the parameter deck 80 high. Visible band bars remain 80 × 14 with larger drag targets. Eight equal-width fluid preset slots remain below the dominant plot. Utility button cuts are 5 px; grouped controls retain simpler geometry. Static insets and side recesses do not receive input.

Native macOS and Windows captures and interaction fixtures validate the actual renderer and hit targets.
