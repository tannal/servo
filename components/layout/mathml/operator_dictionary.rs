/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! W3C MathML Core Compact Operator Dictionary and Intrinsic Stretch Axis table.
//!
//! Generated from the official W3C MathML Core specification tables:
//! - `tables/operator-dictionary-compact.html`
//! - `tables/inline-axis-operators.txt`
//!
//! See <https://w3c.github.io/mathml-core/#operator-dictionary> and
//! <https://w3c.github.io/mathml-core/#algorithms-for-glyph-stretching>.

use app_units::Au;
use malloc_size_of_derive::MallocSizeOf;

/// Operator form used for operator dictionary lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq, MallocSizeOf)]
pub(crate) enum OperatorForm {
    Prefix,
    Infix,
    Postfix,
}

/// MathML Core compact operator dictionary categories (Default + A..=M).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OperatorDictionaryCategory {
    Default,
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
}

/// Resolved default spacing and boolean flags from the operator dictionary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OperatorDictionaryEntry {
    /// Leading space in units of 1/18 em (0..=5).
    pub lspace_eighteenths: u8,
    /// Trailing space in units of 1/18 em (0..=5).
    pub rspace_eighteenths: u8,
    pub stretchy: bool,
    pub symmetric: bool,
    pub largeop: bool,
    pub movablelimits: bool,
    pub accent: bool,
}

impl OperatorDictionaryEntry {
    /// Convert 1/18 em units to scaled `Au` for the given font size.
    #[inline]
    pub(crate) fn space_to_au(eighteenths: u8, font_size: Au) -> Au {
        if eighteenths == 0 {
            Au(0)
        } else {
            Au(((font_size.0 as i64 * eighteenths as i64) + 9) / 18)
        }
    }
}

impl OperatorDictionaryCategory {
    pub(crate) const fn entry(self) -> OperatorDictionaryEntry {
        match self {
            Self::Default => OperatorDictionaryEntry {
                lspace_eighteenths: 5,
                rspace_eighteenths: 5,
                stretchy: false,
                symmetric: false,
                largeop: false,
                movablelimits: false,
                accent: false,
            },
            Self::A => OperatorDictionaryEntry {
                lspace_eighteenths: 5,
                rspace_eighteenths: 5,
                stretchy: true,
                symmetric: false,
                largeop: false,
                movablelimits: false,
                accent: false,
            },
            Self::B => OperatorDictionaryEntry {
                lspace_eighteenths: 4,
                rspace_eighteenths: 4,
                stretchy: false,
                symmetric: false,
                largeop: false,
                movablelimits: false,
                accent: false,
            },
            Self::C => OperatorDictionaryEntry {
                lspace_eighteenths: 3,
                rspace_eighteenths: 3,
                stretchy: false,
                symmetric: false,
                largeop: false,
                movablelimits: false,
                accent: false,
            },
            Self::D | Self::E | Self::K => OperatorDictionaryEntry {
                lspace_eighteenths: 0,
                rspace_eighteenths: 0,
                stretchy: false,
                symmetric: false,
                largeop: false,
                movablelimits: false,
                accent: false,
            },
            Self::F | Self::G => OperatorDictionaryEntry {
                lspace_eighteenths: 0,
                rspace_eighteenths: 0,
                stretchy: true,
                symmetric: true,
                largeop: false,
                movablelimits: false,
                accent: false,
            },
            Self::H => OperatorDictionaryEntry {
                lspace_eighteenths: 3,
                rspace_eighteenths: 3,
                stretchy: false,
                symmetric: true,
                largeop: true,
                movablelimits: false,
                accent: false,
            },
            Self::I => OperatorDictionaryEntry {
                lspace_eighteenths: 0,
                rspace_eighteenths: 0,
                stretchy: true,
                symmetric: false,
                largeop: false,
                movablelimits: false,
                accent: false,
            },
            Self::J => OperatorDictionaryEntry {
                lspace_eighteenths: 3,
                rspace_eighteenths: 3,
                stretchy: false,
                symmetric: true,
                largeop: true,
                movablelimits: true,
                accent: false,
            },
            Self::L => OperatorDictionaryEntry {
                lspace_eighteenths: 3,
                rspace_eighteenths: 0,
                stretchy: false,
                symmetric: false,
                largeop: false,
                movablelimits: false,
                accent: false,
            },
            Self::M => OperatorDictionaryEntry {
                lspace_eighteenths: 0,
                rspace_eighteenths: 3,
                stretchy: false,
                symmetric: false,
                largeop: false,
                movablelimits: false,
                accent: false,
            },
        }
    }
}

fn find_in_ranges(
    ranges: &[(u32, u32, OperatorDictionaryCategory)],
    cp: u32,
) -> Option<OperatorDictionaryCategory> {
    let idx = ranges.partition_point(|&(start, _, _)| start <= cp);
    if idx == 0 {
        return None;
    }
    let (start, end, cat) = ranges[idx - 1];
    if cp >= start && cp <= end {
        Some(cat)
    } else {
        None
    }
}

fn lookup_exact(cp: u32, form: OperatorForm) -> Option<OperatorDictionaryCategory> {
    match form {
        OperatorForm::Infix => find_in_ranges(INFIX_RANGES, cp),
        OperatorForm::Prefix => find_in_ranges(PREFIX_RANGES, cp),
        OperatorForm::Postfix => find_in_ranges(POSTFIX_RANGES, cp),
    }
}

/// Look up an operator's default dictionary entry per MathML Core §3.2.4.2.
///
/// If `(content, form)` is not in the dictionary, falls back to the other forms
/// in order `Infix -> Postfix -> Prefix`, then finally `OperatorDictionaryCategory::Default`.
pub(crate) fn lookup_operator(content: &str, form: OperatorForm) -> OperatorDictionaryEntry {
    // Multi-character operators: check known MathML Core movablelimits words
    // and otherwise return Default (lspace = rspace = 5/18 em).
    let mut chars = content.chars();
    let Some(first) = chars.next() else {
        return OperatorDictionaryCategory::Default.entry();
    };
    if chars.next().is_some() {
        return OperatorDictionaryCategory::Default.entry();
    }

    let cp = first as u32;
    let category = lookup_exact(cp, form)
        .or_else(|| {
            for fallback in [OperatorForm::Infix, OperatorForm::Postfix, OperatorForm::Prefix] {
                if fallback != form {
                    if let Some(cat) = lookup_exact(cp, fallback) {
                        return Some(cat);
                    }
                }
            }
            None
        })
        .unwrap_or(OperatorDictionaryCategory::Default);

    category.entry()
}

/// Returns `true` if `ch` has an inline (horizontal) intrinsic stretch axis
/// according to W3C MathML Core `tables/inline-axis-operators.txt`.
#[inline]
pub(crate) fn is_inline_stretch_operator(ch: char) -> bool {
    INLINE_STRETCH_OPERATORS.binary_search(&(ch as u32)).is_ok()
}

const INFIX_RANGES: &[(u32, u32, OperatorDictionaryCategory)] = &[
    (0x0025, 0x0025, OperatorDictionaryCategory::C),
    (0x002A, 0x002A, OperatorDictionaryCategory::C),
    (0x002B, 0x002B, OperatorDictionaryCategory::B),
    (0x002C, 0x002C, OperatorDictionaryCategory::M),
    (0x002D, 0x002D, OperatorDictionaryCategory::B),
    (0x002E, 0x002E, OperatorDictionaryCategory::C),
    (0x002F, 0x002F, OperatorDictionaryCategory::K),
    (0x003A, 0x003A, OperatorDictionaryCategory::M),
    (0x003B, 0x003B, OperatorDictionaryCategory::M),
    (0x003F, 0x0040, OperatorDictionaryCategory::C),
    (0x005C, 0x005C, OperatorDictionaryCategory::K),
    (0x005E, 0x005E, OperatorDictionaryCategory::C),
    (0x005F, 0x005F, OperatorDictionaryCategory::K),
    (0x00B1, 0x00B1, OperatorDictionaryCategory::B),
    (0x00B7, 0x00B7, OperatorDictionaryCategory::C),
    (0x00D7, 0x00D7, OperatorDictionaryCategory::C),
    (0x00F7, 0x00F7, OperatorDictionaryCategory::B),
    (0x0322, 0x0322, OperatorDictionaryCategory::B),
    (0x0323, 0x0323, OperatorDictionaryCategory::C),
    (0x032E, 0x032E, OperatorDictionaryCategory::C),
    (0x2022, 0x2022, OperatorDictionaryCategory::C),
    (0x2043, 0x2043, OperatorDictionaryCategory::C),
    (0x2044, 0x2044, OperatorDictionaryCategory::B),
    (0x2061, 0x2064, OperatorDictionaryCategory::K),
    (0x2190, 0x2195, OperatorDictionaryCategory::A),
    (0x219A, 0x21AE, OperatorDictionaryCategory::A),
    (0x21B0, 0x21B5, OperatorDictionaryCategory::A),
    (0x21B9, 0x21B9, OperatorDictionaryCategory::A),
    (0x21BC, 0x21D5, OperatorDictionaryCategory::A),
    (0x21DA, 0x21F0, OperatorDictionaryCategory::A),
    (0x21F3, 0x21FF, OperatorDictionaryCategory::A),
    (0x2206, 0x2206, OperatorDictionaryCategory::K),
    (0x2212, 0x2216, OperatorDictionaryCategory::B),
    (0x2217, 0x2219, OperatorDictionaryCategory::C),
    (0x2227, 0x222A, OperatorDictionaryCategory::B),
    (0x2236, 0x2236, OperatorDictionaryCategory::B),
    (0x2238, 0x2238, OperatorDictionaryCategory::B),
    (0x2240, 0x2240, OperatorDictionaryCategory::C),
    (0x228C, 0x228E, OperatorDictionaryCategory::B),
    (0x2293, 0x2296, OperatorDictionaryCategory::B),
    (0x2297, 0x2297, OperatorDictionaryCategory::C),
    (0x2298, 0x2298, OperatorDictionaryCategory::B),
    (0x2299, 0x229B, OperatorDictionaryCategory::C),
    (0x229D, 0x229F, OperatorDictionaryCategory::B),
    (0x22A0, 0x22A1, OperatorDictionaryCategory::C),
    (0x22BA, 0x22BA, OperatorDictionaryCategory::C),
    (0x22BB, 0x22BD, OperatorDictionaryCategory::B),
    (0x22C4, 0x22C7, OperatorDictionaryCategory::C),
    (0x22C9, 0x22CC, OperatorDictionaryCategory::C),
    (0x22CE, 0x22CF, OperatorDictionaryCategory::B),
    (0x22D2, 0x22D3, OperatorDictionaryCategory::B),
    (0x2305, 0x2306, OperatorDictionaryCategory::C),
    (0x2794, 0x2794, OperatorDictionaryCategory::A),
    (0x2795, 0x2797, OperatorDictionaryCategory::B),
    (0x2799, 0x2799, OperatorDictionaryCategory::A),
    (0x279B, 0x27A1, OperatorDictionaryCategory::A),
    (0x27A5, 0x27A6, OperatorDictionaryCategory::A),
    (0x27A8, 0x27AF, OperatorDictionaryCategory::A),
    (0x27B1, 0x27B1, OperatorDictionaryCategory::A),
    (0x27B3, 0x27B3, OperatorDictionaryCategory::A),
    (0x27B5, 0x27B5, OperatorDictionaryCategory::A),
    (0x27B8, 0x27B8, OperatorDictionaryCategory::A),
    (0x27BA, 0x27BE, OperatorDictionaryCategory::A),
    (0x27CB, 0x27CB, OperatorDictionaryCategory::C),
    (0x27CD, 0x27CD, OperatorDictionaryCategory::C),
    (0x27F0, 0x27F1, OperatorDictionaryCategory::A),
    (0x27F4, 0x27FF, OperatorDictionaryCategory::A),
    (0x2900, 0x2920, OperatorDictionaryCategory::A),
    (0x2934, 0x2937, OperatorDictionaryCategory::A),
    (0x2942, 0x2975, OperatorDictionaryCategory::A),
    (0x297C, 0x297F, OperatorDictionaryCategory::A),
    (0x29B8, 0x29B8, OperatorDictionaryCategory::B),
    (0x29BC, 0x29BC, OperatorDictionaryCategory::B),
    (0x29C4, 0x29C5, OperatorDictionaryCategory::B),
    (0x29C6, 0x29C8, OperatorDictionaryCategory::C),
    (0x29D4, 0x29D7, OperatorDictionaryCategory::C),
    (0x29E2, 0x29E2, OperatorDictionaryCategory::C),
    (0x29F5, 0x29FB, OperatorDictionaryCategory::B),
    (0x2A1D, 0x2A1E, OperatorDictionaryCategory::C),
    (0x2A1F, 0x2A2E, OperatorDictionaryCategory::B),
    (0x2A2F, 0x2A37, OperatorDictionaryCategory::C),
    (0x2A38, 0x2A3A, OperatorDictionaryCategory::B),
    (0x2A3B, 0x2A3D, OperatorDictionaryCategory::C),
    (0x2A3E, 0x2A3E, OperatorDictionaryCategory::B),
    (0x2A3F, 0x2A3F, OperatorDictionaryCategory::C),
    (0x2A40, 0x2A4F, OperatorDictionaryCategory::B),
    (0x2A50, 0x2A50, OperatorDictionaryCategory::C),
    (0x2A51, 0x2A63, OperatorDictionaryCategory::B),
    (0x2A64, 0x2A65, OperatorDictionaryCategory::C),
    (0x2ADB, 0x2ADB, OperatorDictionaryCategory::B),
    (0x2ADC, 0x2ADD, OperatorDictionaryCategory::C),
    (0x2AF6, 0x2AF6, OperatorDictionaryCategory::B),
    (0x2AFB, 0x2AFB, OperatorDictionaryCategory::B),
    (0x2AFD, 0x2AFD, OperatorDictionaryCategory::B),
    (0x2AFE, 0x2AFE, OperatorDictionaryCategory::C),
    (0x2B04, 0x2B07, OperatorDictionaryCategory::A),
    (0x2B0C, 0x2B11, OperatorDictionaryCategory::A),
    (0x2B30, 0x2B3E, OperatorDictionaryCategory::A),
    (0x2B40, 0x2B4C, OperatorDictionaryCategory::A),
    (0x2B60, 0x2B65, OperatorDictionaryCategory::A),
    (0x2B6A, 0x2B6D, OperatorDictionaryCategory::A),
    (0x2B70, 0x2B73, OperatorDictionaryCategory::A),
    (0x2B7A, 0x2B7D, OperatorDictionaryCategory::A),
    (0x2B80, 0x2B87, OperatorDictionaryCategory::A),
    (0x2B95, 0x2B95, OperatorDictionaryCategory::A),
    (0x2BA0, 0x2BAF, OperatorDictionaryCategory::A),
    (0x2BB8, 0x2BB8, OperatorDictionaryCategory::A),
];

const PREFIX_RANGES: &[(u32, u32, OperatorDictionaryCategory)] = &[
    (0x0021, 0x0021, OperatorDictionaryCategory::D),
    (0x0028, 0x0028, OperatorDictionaryCategory::F),
    (0x002B, 0x002B, OperatorDictionaryCategory::D),
    (0x002D, 0x002D, OperatorDictionaryCategory::D),
    (0x005B, 0x005B, OperatorDictionaryCategory::F),
    (0x007B, 0x007B, OperatorDictionaryCategory::F),
    (0x007C, 0x007C, OperatorDictionaryCategory::F),
    (0x00AC, 0x00AC, OperatorDictionaryCategory::D),
    (0x00B1, 0x00B1, OperatorDictionaryCategory::D),
    (0x0331, 0x0331, OperatorDictionaryCategory::D),
    (0x2016, 0x2016, OperatorDictionaryCategory::F),
    (0x2018, 0x2018, OperatorDictionaryCategory::D),
    (0x201C, 0x201C, OperatorDictionaryCategory::D),
    (0x2145, 0x2146, OperatorDictionaryCategory::L),
    (0x2200, 0x2201, OperatorDictionaryCategory::D),
    (0x2202, 0x2202, OperatorDictionaryCategory::L),
    (0x2203, 0x2204, OperatorDictionaryCategory::D),
    (0x2207, 0x2207, OperatorDictionaryCategory::D),
    (0x220F, 0x2211, OperatorDictionaryCategory::J),
    (0x2212, 0x2213, OperatorDictionaryCategory::D),
    (0x221A, 0x221C, OperatorDictionaryCategory::L),
    (0x221F, 0x2222, OperatorDictionaryCategory::D),
    (0x222B, 0x2233, OperatorDictionaryCategory::H),
    (0x2234, 0x2235, OperatorDictionaryCategory::D),
    (0x223C, 0x223C, OperatorDictionaryCategory::D),
    (0x22BE, 0x22BF, OperatorDictionaryCategory::D),
    (0x22C0, 0x22C3, OperatorDictionaryCategory::J),
    (0x2308, 0x2308, OperatorDictionaryCategory::F),
    (0x230A, 0x230A, OperatorDictionaryCategory::F),
    (0x2310, 0x2310, OperatorDictionaryCategory::D),
    (0x2319, 0x2319, OperatorDictionaryCategory::D),
    (0x2329, 0x2329, OperatorDictionaryCategory::F),
    (0x2772, 0x2772, OperatorDictionaryCategory::F),
    (0x2795, 0x2796, OperatorDictionaryCategory::D),
    (0x27C0, 0x27C0, OperatorDictionaryCategory::D),
    (0x27E6, 0x27E6, OperatorDictionaryCategory::F),
    (0x27E8, 0x27E8, OperatorDictionaryCategory::F),
    (0x27EA, 0x27EA, OperatorDictionaryCategory::F),
    (0x27EC, 0x27EC, OperatorDictionaryCategory::F),
    (0x27EE, 0x27EE, OperatorDictionaryCategory::F),
    (0x2980, 0x2980, OperatorDictionaryCategory::F),
    (0x2983, 0x2983, OperatorDictionaryCategory::F),
    (0x2985, 0x2985, OperatorDictionaryCategory::F),
    (0x2987, 0x2987, OperatorDictionaryCategory::F),
    (0x2989, 0x2989, OperatorDictionaryCategory::F),
    (0x298B, 0x298B, OperatorDictionaryCategory::F),
    (0x298D, 0x298D, OperatorDictionaryCategory::F),
    (0x298F, 0x298F, OperatorDictionaryCategory::F),
    (0x2991, 0x2991, OperatorDictionaryCategory::F),
    (0x2993, 0x2993, OperatorDictionaryCategory::F),
    (0x2995, 0x2995, OperatorDictionaryCategory::F),
    (0x2997, 0x2997, OperatorDictionaryCategory::F),
    (0x2999, 0x2999, OperatorDictionaryCategory::F),
    (0x299B, 0x29AF, OperatorDictionaryCategory::D),
    (0x29D8, 0x29D8, OperatorDictionaryCategory::F),
    (0x29DA, 0x29DA, OperatorDictionaryCategory::F),
    (0x29FC, 0x29FC, OperatorDictionaryCategory::F),
    (0x2A00, 0x2A0A, OperatorDictionaryCategory::J),
    (0x2A0B, 0x2A1C, OperatorDictionaryCategory::H),
    (0x2A1D, 0x2A1E, OperatorDictionaryCategory::J),
    (0x2AEC, 0x2AED, OperatorDictionaryCategory::D),
    (0x2AFC, 0x2AFC, OperatorDictionaryCategory::J),
    (0x2AFF, 0x2AFF, OperatorDictionaryCategory::J),
];

const POSTFIX_RANGES: &[(u32, u32, OperatorDictionaryCategory)] = &[
    (0x0021, 0x0022, OperatorDictionaryCategory::E),
    (0x0025, 0x0027, OperatorDictionaryCategory::E),
    (0x0029, 0x0029, OperatorDictionaryCategory::G),
    (0x005D, 0x005D, OperatorDictionaryCategory::G),
    (0x005E, 0x005F, OperatorDictionaryCategory::I),
    (0x0060, 0x0060, OperatorDictionaryCategory::E),
    (0x007C, 0x007C, OperatorDictionaryCategory::G),
    (0x007D, 0x007D, OperatorDictionaryCategory::G),
    (0x007E, 0x007E, OperatorDictionaryCategory::I),
    (0x00A8, 0x00A8, OperatorDictionaryCategory::E),
    (0x00AF, 0x00AF, OperatorDictionaryCategory::I),
    (0x00B0, 0x00B0, OperatorDictionaryCategory::E),
    (0x00B2, 0x00B4, OperatorDictionaryCategory::E),
    (0x00B8, 0x00B9, OperatorDictionaryCategory::E),
    (0x02C6, 0x02C7, OperatorDictionaryCategory::I),
    (0x02C9, 0x02C9, OperatorDictionaryCategory::I),
    (0x02CA, 0x02CB, OperatorDictionaryCategory::E),
    (0x02CD, 0x02CD, OperatorDictionaryCategory::I),
    (0x02D8, 0x02DA, OperatorDictionaryCategory::E),
    (0x02DC, 0x02DC, OperatorDictionaryCategory::I),
    (0x02DD, 0x02DD, OperatorDictionaryCategory::E),
    (0x02F7, 0x02F7, OperatorDictionaryCategory::I),
    (0x0302, 0x0302, OperatorDictionaryCategory::I),
    (0x0311, 0x0311, OperatorDictionaryCategory::E),
    (0x0320, 0x0320, OperatorDictionaryCategory::E),
    (0x0325, 0x0325, OperatorDictionaryCategory::E),
    (0x0327, 0x0327, OperatorDictionaryCategory::E),
    (0x0331, 0x0331, OperatorDictionaryCategory::E),
    (0x2016, 0x2016, OperatorDictionaryCategory::G),
    (0x2019, 0x201B, OperatorDictionaryCategory::E),
    (0x201D, 0x201F, OperatorDictionaryCategory::E),
    (0x2032, 0x2037, OperatorDictionaryCategory::E),
    (0x203E, 0x203E, OperatorDictionaryCategory::I),
    (0x2057, 0x2057, OperatorDictionaryCategory::E),
    (0x20DB, 0x20DC, OperatorDictionaryCategory::E),
    (0x2309, 0x2309, OperatorDictionaryCategory::G),
    (0x230B, 0x230B, OperatorDictionaryCategory::G),
    (0x2322, 0x2323, OperatorDictionaryCategory::I),
    (0x232A, 0x232A, OperatorDictionaryCategory::G),
    (0x23B4, 0x23B5, OperatorDictionaryCategory::I),
    (0x23CD, 0x23CD, OperatorDictionaryCategory::E),
    (0x23DC, 0x23E1, OperatorDictionaryCategory::I),
    (0x2773, 0x2773, OperatorDictionaryCategory::G),
    (0x27E7, 0x27E7, OperatorDictionaryCategory::G),
    (0x27E9, 0x27E9, OperatorDictionaryCategory::G),
    (0x27EB, 0x27EB, OperatorDictionaryCategory::G),
    (0x27ED, 0x27ED, OperatorDictionaryCategory::G),
    (0x27EF, 0x27EF, OperatorDictionaryCategory::G),
    (0x2980, 0x2980, OperatorDictionaryCategory::G),
    (0x2984, 0x2984, OperatorDictionaryCategory::G),
    (0x2986, 0x2986, OperatorDictionaryCategory::G),
    (0x2988, 0x2988, OperatorDictionaryCategory::G),
    (0x298A, 0x298A, OperatorDictionaryCategory::G),
    (0x298C, 0x298C, OperatorDictionaryCategory::G),
    (0x298E, 0x298E, OperatorDictionaryCategory::G),
    (0x2990, 0x2990, OperatorDictionaryCategory::G),
    (0x2992, 0x2992, OperatorDictionaryCategory::G),
    (0x2994, 0x2994, OperatorDictionaryCategory::G),
    (0x2996, 0x2996, OperatorDictionaryCategory::G),
    (0x2998, 0x2998, OperatorDictionaryCategory::G),
    (0x2999, 0x2999, OperatorDictionaryCategory::G),
    (0x29D9, 0x29D9, OperatorDictionaryCategory::G),
    (0x29DB, 0x29DB, OperatorDictionaryCategory::G),
    (0x29FD, 0x29FD, OperatorDictionaryCategory::G),
];

const INLINE_STRETCH_OPERATORS: &[u32] = &[
    0x003D, 0x005E, 0x005F, 0x007E, 0x00AF, 0x02C6, 0x02C7, 0x02C9, 0x02CD, 0x02DC,
    0x02F7, 0x0302, 0x0332, 0x203E, 0x20D0, 0x20D1, 0x20D6, 0x20D7, 0x20E1, 0x2190,
    0x2192, 0x2194, 0x2198, 0x2199, 0x219A, 0x219B, 0x219C, 0x219D, 0x219E, 0x21A0,
    0x21A2, 0x21A3, 0x21A4, 0x21A6, 0x21A9, 0x21AA, 0x21AB, 0x21AC, 0x21AD, 0x21AE,
    0x21B4, 0x21B9, 0x21BC, 0x21BD, 0x21C0, 0x21C1, 0x21C4, 0x21C6, 0x21C7, 0x21C9,
    0x21CB, 0x21CC, 0x21CD, 0x21CE, 0x21CF, 0x21D0, 0x21D2, 0x21D4, 0x21DA, 0x21DB,
    0x21DC, 0x21DD, 0x21E0, 0x21E2, 0x21E4, 0x21E5, 0x21E6, 0x21E8, 0x21F0, 0x21F4,
    0x21F6, 0x21F7, 0x21F8, 0x21F9, 0x21FA, 0x21FB, 0x21FC, 0x21FD, 0x21FE, 0x21FF,
    0x2322, 0x2323, 0x23B4, 0x23B5, 0x23DC, 0x23DD, 0x23DE, 0x23DF, 0x23E0, 0x23E1,
    0x2500, 0x2794, 0x2799, 0x279B, 0x279C, 0x279D, 0x279E, 0x279F, 0x27A0, 0x27A1,
    0x27A5, 0x27A6, 0x27A8, 0x27A9, 0x27AA, 0x27AB, 0x27AC, 0x27AD, 0x27AE, 0x27AF,
    0x27B1, 0x27B3, 0x27B5, 0x27B8, 0x27BA, 0x27BB, 0x27BC, 0x27BD, 0x27BE, 0x27F4,
    0x27F5, 0x27F6, 0x27F7, 0x27F8, 0x27F9, 0x27FA, 0x27FB, 0x27FC, 0x27FD, 0x27FE,
    0x27FF, 0x2900, 0x2901, 0x2902, 0x2903, 0x2904, 0x2905, 0x2906, 0x2907, 0x290C,
    0x290D, 0x290E, 0x290F, 0x2910, 0x2911, 0x2914, 0x2915, 0x2916, 0x2917, 0x2918,
    0x2919, 0x291A, 0x291B, 0x291C, 0x291D, 0x291E, 0x291F, 0x2920, 0x2942, 0x2943,
    0x2944, 0x2945, 0x2946, 0x2947, 0x2948, 0x294A, 0x294B, 0x294E, 0x2950, 0x2952,
    0x2953, 0x2956, 0x2957, 0x295A, 0x295B, 0x295E, 0x295F, 0x2962, 0x2964, 0x2966,
    0x2967, 0x2968, 0x2969, 0x296A, 0x296B, 0x296C, 0x296D, 0x2970, 0x2971, 0x2972,
    0x2973, 0x2974, 0x2975, 0x297C, 0x297D, 0x2B04, 0x2B05, 0x2B0C, 0x2B30, 0x2B31,
    0x2B32, 0x2B33, 0x2B34, 0x2B35, 0x2B36, 0x2B37, 0x2B38, 0x2B39, 0x2B3A, 0x2B3B,
    0x2B3C, 0x2B3D, 0x2B3E, 0x2B40, 0x2B41, 0x2B42, 0x2B43, 0x2B44, 0x2B45, 0x2B46,
    0x2B47, 0x2B48, 0x2B49, 0x2B4A, 0x2B4B, 0x2B4C, 0x2B60, 0x2B62, 0x2B64, 0x2B6A,
    0x2B6C, 0x2B70, 0x2B72, 0x2B7A, 0x2B7C, 0x2B80, 0x2B82, 0x2B84, 0x2B86, 0x2B95,
    0xFE35, 0xFE36, 0xFE37, 0xFE38, 0x1EEF0, 0x1EEF1,
];
