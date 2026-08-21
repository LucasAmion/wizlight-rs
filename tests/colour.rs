//! The RGB+CW conversion, held against the algorithm it was ported from.
//!
//! Two kinds of check live here. The first replays a table of values recorded
//! from `pywizlight` (`tests/data/rgbcw_golden.json`, written by the script
//! beside it) and requires the port to reproduce them. The second states
//! properties that must hold everywhere, not just at the recorded points, and
//! sweeps a dense grid to look for a counterexample.
//!
//! The properties are checked by sweeping rather than by a property-testing
//! crate. The input space is two bounded floats and three bytes, so a grid
//! covers it more thoroughly than random sampling would, and it does it
//! identically on every run — a failure here is always reproducible, and the
//! crate keeps its dependency list.

use serde_json::Value;
use wizlight::protocol::{CW_MAX, ColourStrategy, Hs, Rgbcw, WhiteChannel};

/// How far a channel may fall from the recorded value when the conversion
/// went through `cos` and `sin`.
///
/// Those are the only operations in it that a platform is free to round its
/// own way; everything else — add, multiply, divide, `sqrt` — is exact by
/// IEEE-754. This runs on three `libm` implementations in CI, and a
/// difference in the last bit is enough to move a truncated result by one.
///
/// The cost of the allowance is that it cannot see a change of exactly one,
/// so it must not be used where it is not needed: see
/// [`rgb_to_rgbcw_matches_pywizlight`], which is exact for that reason.
const CHANNEL_TOLERANCE: i32 = 1;

/// The same, for a conversion that never leaves exact arithmetic.
const EXACT: i32 = 0;

/// How far a recovered hue or saturation may fall from the recorded value.
///
/// Nothing is truncated on this side, so only the last bits of the float can
/// differ, and this is orders of magnitude looser than what is observed.
const ANGLE_TOLERANCE: f64 = 1e-9;

/// The recorded answers.
fn golden() -> Value {
    let raw = include_str!("data/rgbcw_golden.json");
    serde_json::from_str(raw).expect("the golden table is JSON")
}

fn rows(table: &Value, key: &str) -> Vec<(Vec<f64>, Vec<f64>)> {
    let numbers = |value: &Value| -> Vec<f64> {
        value
            .as_array()
            .expect("an entry is a pair of arrays")
            .iter()
            .map(|number| number.as_f64().expect("entries are numbers"))
            .collect()
    };
    table[key]
        .as_array()
        .unwrap_or_else(|| panic!("the table has a `{key}` section"))
        .iter()
        .map(|row| (numbers(&row[0]), numbers(&row[1])))
        .collect()
}

/// A channel value out of the table, which holds them as JSON numbers.
fn byte(value: f64) -> u8 {
    value as u8
}

/// Fails with everything needed to see what drifted, rather than on the first
/// row: a port that is off by one everywhere is a different problem from one
/// that is wrong in a corner, and the difference should be visible at a
/// glance.
fn report(mismatches: &[String], total: usize, what: &str) {
    assert!(
        mismatches.is_empty(),
        "{} of {total} {what} conversions disagree with pywizlight:\n{}",
        mismatches.len(),
        mismatches
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

fn channels_within(
    got: Rgbcw,
    want: &[f64],
    white: WhiteChannel,
    tolerance: i32,
) -> Option<String> {
    let want_white = byte(want[3]);
    let (want_c, want_w) = match white {
        WhiteChannel::Cold => (want_white, 0),
        WhiteChannel::Warm => (0, want_white),
    };
    let expected = Rgbcw {
        r: byte(want[0]),
        g: byte(want[1]),
        b: byte(want[2]),
        c: want_c,
        w: want_w,
    };
    let off = |got: u8, want: u8| (i32::from(got) - i32::from(want)).abs() > tolerance;
    (off(got.r, expected.r)
        || off(got.g, expected.g)
        || off(got.b, expected.b)
        || off(got.c, expected.c)
        || off(got.w, expected.w))
    .then(|| format!("  got {got:?}, want {expected:?}"))
}

/// `rgb2rgbcw`, on the recorded inputs, **exactly**.
///
/// This path never calls a transcendental function: the primaries are
/// constants, and the hue vector, its length and the gamut rescale are all
/// add, multiply, divide and `sqrt`, which IEEE-754 pins to the last bit. So
/// every platform must produce the recorded byte, and the port's rounding —
/// `pywizlight` truncates where it might have rounded — is pinned with it.
#[test]
fn rgb_to_rgbcw_matches_pywizlight() {
    let table = golden();
    let rows = rows(&table, "rgb_to_rgbcw");
    let mut mismatches = Vec::new();
    for (input, want) in &rows {
        let rgb = (byte(input[0]), byte(input[1]), byte(input[2]));
        let got = ColourStrategy::Trapezoid(WhiteChannel::Cold).apply_rgb(rgb);
        if let Some(detail) = channels_within(got, want, WhiteChannel::Cold, EXACT) {
            mismatches.push(format!("rgb {rgb:?}:\n{detail}"));
        }
    }
    report(&mismatches, rows.len(), "rgb2rgbcw");
}

/// `hs2rgbcw`, on the recorded inputs — the entry point the algorithm is
/// written for, and the one that turns a hue into a vector with `cos` and
/// `sin`. Hence the one-channel allowance; see [`CHANNEL_TOLERANCE`].
#[test]
fn hs_to_rgbcw_matches_pywizlight() {
    let table = golden();
    let rows = rows(&table, "hs_to_rgbcw");
    let mut mismatches = Vec::new();
    for (input, want) in &rows {
        let (hue, saturation) = (input[0], input[1]);
        let hs = Hs::new(hue, saturation).expect("the grid is in range");
        // The warm spelling is exercised here so that both placements are
        // covered by the golden table, not just by the unit tests.
        let got = ColourStrategy::Trapezoid(WhiteChannel::Warm).apply_hs(hs);
        if let Some(detail) = channels_within(got, want, WhiteChannel::Warm, CHANNEL_TOLERANCE) {
            mismatches.push(format!("hs ({hue}, {saturation}):\n{detail}"));
        }
    }
    report(&mismatches, rows.len(), "hs2rgbcw");
}

/// `rgbcw2hs`, on the recorded inputs.
#[test]
fn rgbcw_to_hs_matches_pywizlight() {
    let table = golden();
    let rows = rows(&table, "rgbcw_to_hs");
    let mut mismatches = Vec::new();
    for (input, want) in &rows {
        let colour = Rgbcw {
            r: byte(input[0]),
            g: byte(input[1]),
            b: byte(input[2]),
            c: byte(input[3]),
            w: 0,
        };
        let got = colour.to_hs();
        let (want_hue, want_saturation) = (want[0], want[1]);
        // 360 and 0 are the same hue, and which one comes out is down to the
        // last bit of an `atan2`.
        let hue_delta = (got.hue() - want_hue)
            .abs()
            .min((got.hue() + 360.0 - want_hue).abs());
        if hue_delta > ANGLE_TOLERANCE
            || (got.saturation() - want_saturation).abs() > ANGLE_TOLERANCE
        {
            mismatches.push(format!(
                "rgbcw {colour:?}: got ({}, {}), want ({want_hue}, {want_saturation})",
                got.hue(),
                got.saturation(),
            ));
        }
    }
    report(&mismatches, rows.len(), "rgbcw2hs");
}

/// The table has to say where it came from, or a future reader cannot tell
/// whether it is still describing anything real.
#[test]
fn the_golden_table_records_its_provenance() {
    let table = golden();
    assert_eq!(table["source"]["project"], "pywizlight");
    assert!(
        table["source"]["version"]
            .as_str()
            .is_some_and(|v| v != "unknown"),
        "the table should name the pywizlight version it came from",
    );
    assert!(
        table["generator"]
            .as_str()
            .is_some_and(|g| g.ends_with(".py"))
    );
}

/// Every hue and saturation, at 0.25° and 0.5% — 1.04M colours per strategy.
fn sweep(mut check: impl FnMut(Hs)) {
    for hue_step in 0..1440 {
        for saturation_step in 0..=200 {
            let hs = Hs::new(f64::from(hue_step) * 0.25, f64::from(saturation_step) * 0.5)
                .expect("the sweep is in range");
            check(hs);
        }
    }
}

/// The white channel is never driven past what the algorithm promises, and
/// only ever one of the two is lit.
#[test]
fn a_blend_lights_one_white_and_never_past_its_ceiling() {
    for white in [WhiteChannel::Cold, WhiteChannel::Warm] {
        sweep(|hs| {
            let out = ColourStrategy::Trapezoid(white).apply_hs(hs);
            assert!(out.c <= CW_MAX && out.w <= CW_MAX, "{hs:?} -> {out:?}");
            assert!(out.c == 0 || out.w == 0, "{hs:?} lit both whites: {out:?}");
            match white {
                WhiteChannel::Cold => assert_eq!(out.w, 0),
                WhiteChannel::Warm => assert_eq!(out.c, 0),
            }
        });
    }
}

/// Raw RGB never touches a white channel, whatever it is asked for.
#[test]
fn raw_rgb_leaves_both_whites_dark() {
    sweep(|hs| {
        let out = ColourStrategy::Rgb.apply_hs(hs);
        assert_eq!((out.c, out.w), (0, 0), "{hs:?} -> {out:?}");
    });
    for r in 0..=255u8 {
        let out = ColourStrategy::Rgb.apply_rgb((r, 255 - r, r / 2));
        assert_eq!(
            out,
            Rgbcw {
                r,
                g: 255 - r,
                b: r / 2,
                c: 0,
                w: 0
            }
        );
    }
}

/// At least one emitter is lit for every colour: nothing converts to darkness,
/// which would be a colour request that silently turned the bulb off.
#[test]
fn no_colour_converts_to_nothing() {
    for white in [WhiteChannel::Cold, WhiteChannel::Warm] {
        sweep(|hs| {
            let out = ColourStrategy::Trapezoid(white).apply_hs(hs);
            assert!(
                out.r > 0 || out.g > 0 || out.b > 0 || out.c > 0 || out.w > 0,
                "{hs:?} -> {out:?}",
            );
        });
    }
    sweep(|hs| {
        let out = ColourStrategy::Rgb.apply_hs(hs);
        assert!(out.r > 0 || out.g > 0 || out.b > 0, "{hs:?} -> {out:?}");
    });
}

/// `hs → rgbcw → hs` is stable, within bounds this test pins.
///
/// The inverse is not exact, and the two halves of the trapezoid lose
/// different things:
///
/// - **above saturation 0.5** the white channel carries the saturation, and
///   the only loss is its own quantisation — one part in 128, so 0.375 of a
///   percentage point at worst;
/// - **at 0.5 and below** the white is pinned at its ceiling and saturation is
///   read back out of the *length* of the colour vector, which the forward
///   conversion had rescaled to fit the gamut. Nothing undoes that rescale, so
///   the answer comes back low — by up to 13.4% for a hue midway between two
///   primaries, which is the hexagon edge dipping to `cos(30°)` of its
///   corners, and by more in relative terms once byte quantisation bites.
///
/// Hue survives well while the colour channels carry any signal, and stops
/// meaning much as they approach zero: at 1% saturation the colour is two or
/// three units of RGB on top of a full white, and there is not enough left of
/// it to say what hue it was.
///
/// Every bound below is **measured by this sweep**, not chosen. They are what
/// the algorithm does; they are here so that changing it has to move them.
#[test]
fn hs_round_trips_within_a_known_error() {
    let (mut above, mut at_or_below) = (0.0f64, 0.0f64);
    // Hue error, worst case over saturations at or above 1, 5, 10, 25 and 50.
    let floors = [1.0, 5.0, 10.0, 25.0, 50.0];
    let mut hue_error = [0.0f64; 5];

    sweep(|hs| {
        let out = ColourStrategy::Trapezoid(WhiteChannel::Cold).apply_hs(hs);
        let back = out.to_hs();

        let drift = (back.saturation() - hs.saturation()).abs();
        if hs.saturation() > 50.0 {
            above = above.max(drift);
        } else {
            at_or_below = at_or_below.max(drift);
        }

        // Hue is meaningless at zero saturation: the colour is a white.
        if hs.saturation() > 0.0 {
            let delta = (back.hue() - hs.hue()).abs();
            let delta = delta.min(360.0 - delta);
            for (index, floor) in floors.iter().enumerate() {
                if hs.saturation() >= *floor {
                    hue_error[index] = hue_error[index].max(delta);
                }
            }
        }
    });

    assert!(
        above <= 0.375,
        "saturation drifted by {above} above the step"
    );
    assert!(
        at_or_below <= 6.72,
        "saturation drifted by {at_or_below} at or below the step",
    );
    for (index, limit) in [12.34, 2.26, 1.29, 0.5, 0.26].into_iter().enumerate() {
        assert!(
            hue_error[index] <= limit,
            "hue drifted by {}° at saturation >= {}",
            hue_error[index],
            floors[index],
        );
    }
}

/// Above the step the conversion is a **fixed point**: send a colour, read the
/// bulb, convert what it reports, and the same channels come back.
///
/// At and below the step it is not, and this pins that too. Saturation is
/// recovered from a vector the forward conversion shortened, so each round
/// trip pales the colour a little more — a client that re-converted its own
/// readback in a loop would watch the colour drain away. Read the state you
/// asked for from your own request, not from the bulb.
#[test]
fn the_blend_is_a_fixed_point_only_above_the_step() {
    let mut mismatches = Vec::new();
    let (mut total, mut worst_below) = (0, 0);
    sweep(|hs| {
        let strategy = ColourStrategy::Trapezoid(WhiteChannel::Cold);
        let out = strategy.apply_hs(hs);
        let again = strategy.apply_hs(out.to_hs());
        let drift = |a: u8, b: u8| (i32::from(a) - i32::from(b)).abs();
        let worst = drift(again.r, out.r)
            .max(drift(again.g, out.g))
            .max(drift(again.b, out.b))
            .max(drift(again.c, out.c))
            .max(drift(again.w, out.w));
        if hs.saturation() > 50.0 {
            total += 1;
            if worst > CHANNEL_TOLERANCE {
                mismatches.push(format!("  {hs:?}: {out:?} -> {again:?}"));
            }
        } else {
            worst_below = worst_below.max(worst);
        }
    });
    report(&mismatches, total, "fixed-point");
    assert_eq!(
        worst_below, 35,
        "the loss below the step has changed; it was 35 of 255, at saturation 50 exactly",
    );
}
