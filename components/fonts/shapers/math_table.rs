/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Pure-Rust OpenType `MATH` table reader using `skrifa` + raw `MATH` table bytes.
//!
//! Provides OpenType MATH constants, font scales, glyph ink extents, italics
//! correction, top accent attachment, and size-variant / multi-part glyph
//! stretching for the HarfRust shaper (Servo's default shaping backend).
//!
//! Specification: <https://learn.microsoft.com/en-us/typography/opentype/spec/math>

use app_units::Au;
use num_traits::Zero as _;
use read_fonts::types::GlyphId;
use skrifa::Font as SkrifaFont;

use crate::{
    MathConstants, MathFontScales, MathGlyphExtents, MathStretchDirection, PositionedMathGlyph,
    StretchedMathGlyph,
};

const EXTENDER_FLAG: u16 = 0x0001;

#[inline]
fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    let bytes: [u8; 2] = data.get(offset..offset + 2)?.try_into().ok()?;
    Some(u16::from_be_bytes(bytes))
}

#[inline]
fn read_i16(data: &[u8], offset: usize) -> Option<i16> {
    let bytes: [u8; 2] = data.get(offset..offset + 2)?.try_into().ok()?;
    Some(i16::from_be_bytes(bytes))
}

/// Read the `value` field of a 4-byte `MathValueRecord` (`value: i16`, `deviceOffset: u16`).
#[inline]
fn read_math_value_record(data: &[u8], offset: usize) -> Option<i16> {
    read_i16(data, offset)
}

/// Look up `glyph_id` in an OpenType Coverage table (Format 1 or Format 2).
fn coverage_index(table: &[u8], glyph_id: u16) -> Option<usize> {
    let format = read_u16(table, 0)?;
    match format {
        1 => {
            let count = read_u16(table, 2)? as usize;
            let mut lo = 0usize;
            let mut hi = count;
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                let g = read_u16(table, 4 + mid * 2)?;
                if g < glyph_id {
                    lo = mid + 1;
                } else if g > glyph_id {
                    hi = mid;
                } else {
                    return Some(mid);
                }
            }
            None
        },
        2 => {
            let range_count = read_u16(table, 2)? as usize;
            let mut lo = 0usize;
            let mut hi = range_count;
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                let base = 4 + mid * 6;
                let start = read_u16(table, base)?;
                let end = read_u16(table, base + 2)?;
                if glyph_id < start {
                    hi = mid;
                } else if glyph_id > end {
                    lo = mid + 1;
                } else {
                    let start_coverage_index = read_u16(table, base + 4)? as usize;
                    return Some(start_coverage_index + (glyph_id - start) as usize);
                }
            }
            None
        },
        _ => None,
    }
}

struct MathTableView<'a> {
    data: &'a [u8],
    constants_offset: usize,
    glyph_info_offset: usize,
    variants_offset: usize,
}

impl<'a> MathTableView<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        let major = read_u16(data, 0)?;
        if major != 1 {
            return None;
        }
        let constants_offset = read_u16(data, 4)? as usize;
        let glyph_info_offset = read_u16(data, 6)? as usize;
        let variants_offset = read_u16(data, 8)? as usize;
        Some(Self {
            data,
            constants_offset,
            glyph_info_offset,
            variants_offset,
        })
    }

    fn constants_slice(&self) -> Option<&'a [u8]> {
        if self.constants_offset == 0 {
            return None;
        }
        self.data.get(self.constants_offset..)
    }

    fn glyph_info_slice(&self) -> Option<&'a [u8]> {
        if self.glyph_info_offset == 0 {
            return None;
        }
        self.data.get(self.glyph_info_offset..)
    }

    fn variants_slice(&self) -> Option<&'a [u8]> {
        if self.variants_offset == 0 {
            return None;
        }
        self.data.get(self.variants_offset..)
    }

    fn glyph_italics_correction(&self, glyph_id: u16) -> Option<i16> {
        let gi = self.glyph_info_slice()?;
        let ic_offset = read_u16(gi, 0)? as usize;
        if ic_offset == 0 {
            return None;
        }
        let ic_table = gi.get(ic_offset..)?;
        let cov_offset = read_u16(ic_table, 0)? as usize;
        let count = read_u16(ic_table, 2)? as usize;
        let cov = ic_table.get(cov_offset..)?;
        let idx = coverage_index(cov, glyph_id)?;
        if idx >= count {
            return None;
        }
        read_math_value_record(ic_table, 4 + idx * 4)
    }

    fn glyph_top_accent_attachment(&self, glyph_id: u16) -> Option<i16> {
        let gi = self.glyph_info_slice()?;
        let accent_offset = read_u16(gi, 2)? as usize;
        if accent_offset == 0 {
            return None;
        }
        let accent_table = gi.get(accent_offset..)?;
        let cov_offset = read_u16(accent_table, 0)? as usize;
        let count = read_u16(accent_table, 2)? as usize;
        let cov = accent_table.get(cov_offset..)?;
        let idx = coverage_index(cov, glyph_id)?;
        if idx >= count {
            return None;
        }
        read_math_value_record(accent_table, 4 + idx * 4)
    }

    fn glyph_construction(
        &self,
        glyph_id: u16,
        direction: MathStretchDirection,
    ) -> Option<(u16, &'a [u8])> {
        let variants = self.variants_slice()?;
        let min_connector_overlap = read_u16(variants, 0)?;
        let vert_cov_offset = read_u16(variants, 2)? as usize;
        let horiz_cov_offset = read_u16(variants, 4)? as usize;
        let vert_count = read_u16(variants, 6)? as usize;
        let horiz_count = read_u16(variants, 8)? as usize;

        let (cov_offset, count, array_start) = match direction {
            MathStretchDirection::Vertical => (vert_cov_offset, vert_count, 10),
            MathStretchDirection::Horizontal => {
                (horiz_cov_offset, horiz_count, 10 + vert_count * 2)
            },
        };
        if cov_offset == 0 || count == 0 {
            return None;
        }
        let cov = variants.get(cov_offset..)?;
        let idx = coverage_index(cov, glyph_id)?;
        if idx >= count {
            return None;
        }
        let construction_offset = read_u16(variants, array_start + idx * 2)? as usize;
        if construction_offset == 0 {
            return None;
        }
        let construction = variants.get(construction_offset..)?;
        Some((min_connector_overlap, construction))
    }
}

#[derive(Clone, Copy)]
struct RawGlyphPart {
    glyph_id: u32,
    start_connector_length: u16,
    end_connector_length: u16,
    full_advance: u16,
    flags: u16,
}

#[inline]
fn scale_funit_to_au(value: i32, ppem: f64, units_per_em: u16) -> Au {
    if units_per_em == 0 {
        return Au::zero();
    }
    Au::from_f64_px(value as f64 * ppem / units_per_em as f64)
}

fn glyph_extents_by_id(skrifa_font: &SkrifaFont, ppem: f64, glyph_id: u32) -> MathGlyphExtents {
    let gm = skrifa_font.glyph_metrics().scaled_f32(ppem as f32);
    let gid = GlyphId::new(glyph_id);
    let advance = Au::from_f32_px(gm.h_advance(gid));
    if let Some(ext) = gm.extents(gid) {
        MathGlyphExtents {
            advance,
            ascent: Au::from_f32_px(ext.y_bearing),
            descent: Au::from_f32_px(ext.height - ext.y_bearing),
            x_bearing: Au::from_f32_px(ext.x_bearing),
            width: Au::from_f32_px(ext.width.max(0.0)),
        }
    } else {
        MathGlyphExtents {
            advance,
            ascent: Au::zero(),
            descent: Au::zero(),
            x_bearing: Au::zero(),
            width: Au::zero(),
        }
    }
}

pub(crate) fn math_font_scales(math_data: Option<&[u8]>) -> Option<MathFontScales> {
    let view = MathTableView::parse(math_data?)?;
    let c = view.constants_slice()?;
    let raw_script = read_i16(c, 0)?;
    let raw_script_script = read_i16(c, 2)?;
    let to_scale = |percent: i16| {
        if percent > 0 {
            Some(percent as f32 / 100.0)
        } else {
            None
        }
    };
    Some(MathFontScales {
        script_percent_scale_down: to_scale(raw_script),
        script_script_percent_scale_down: to_scale(raw_script_script),
    })
}

pub(crate) fn math_constants(
    skrifa_font: &SkrifaFont,
    math_data: Option<&[u8]>,
    ppem: f64,
    is_display: bool,
) -> Option<MathConstants> {
    let units_per_em = skrifa_font.units_per_em();
    if units_per_em == 0 {
        return None;
    }
    let view = MathTableView::parse(math_data?)?;
    let c = view.constants_slice()?;

    // Verify table is large enough to cover radicalDegreeBottomRaisePercent at offset 212..214.
    if c.len() < 214 {
        return None;
    }

    let u16_au = |offset: usize| -> Au {
        scale_funit_to_au(read_u16(c, offset).unwrap_or(0) as i32, ppem, units_per_em)
    };
    let rec_au = |index: usize| -> Au {
        let offset = 8 + index * 4;
        scale_funit_to_au(
            read_math_value_record(c, offset).unwrap_or(0) as i32,
            ppem,
            units_per_em,
        )
    };

    let (
        num_shift_up,
        denom_shift_down,
        num_gap_min,
        denom_gap_min,
        stack_top_shift_up,
        stack_bottom_shift_down,
        stack_gap_min,
        radical_vertical_gap,
    ) = if is_display {
        (
            rec_au(29), // fractionNumeratorDisplayStyleShiftUp
            rec_au(31), // fractionDenominatorDisplayStyleShiftDown
            rec_au(33), // fractionNumDisplayStyleGapMin
            rec_au(36), // fractionDenomDisplayStyleGapMin
            rec_au(19), // stackTopDisplayStyleShiftUp
            rec_au(21), // stackBottomDisplayStyleShiftDown
            rec_au(23), // stackDisplayStyleGapMin
            rec_au(46), // radicalDisplayStyleVerticalGap
        )
    } else {
        (
            rec_au(28), // fractionNumeratorShiftUp
            rec_au(30), // fractionDenominatorShiftDown
            rec_au(32), // fractionNumeratorGapMin
            rec_au(35), // fractionDenominatorGapMin
            rec_au(18), // stackTopShiftUp
            rec_au(20), // stackBottomShiftDown
            rec_au(22), // stackGapMin
            rec_au(45), // radicalVerticalGap
        )
    };

    Some(MathConstants {
        axis_height: rec_au(1),
        display_operator_min_height: u16_au(6),
        delimited_sub_formula_min_height: u16_au(4),
        accent_base_height: rec_au(2),
        flattened_accent_base_height: rec_au(3),

        rule_thickness: rec_au(34),
        num_shift_up,
        num_gap_min,
        denom_shift_down,
        denom_gap_min,
        stack_top_shift_up,
        stack_bottom_shift_down,
        stack_gap_min,

        radical_vertical_gap,
        radical_rule_thickness: rec_au(47),
        radical_extra_ascender: rec_au(48),
        radical_kern_before_degree: rec_au(49),
        radical_kern_after_degree: rec_au(50),
        radical_degree_bottom_raise_percent: read_i16(c, 212).unwrap_or(60) as i32,

        subscript_shift_down: rec_au(4),
        subscript_top_max: rec_au(5),
        subscript_baseline_drop_min: rec_au(6),
        superscript_shift_up: rec_au(7),
        superscript_shift_up_cramped: rec_au(8),
        superscript_bottom_min: rec_au(9),
        superscript_baseline_drop_max: rec_au(10),
        sub_superscript_gap_min: rec_au(11),
        superscript_bottom_max_with_subscript: rec_au(12),
        space_after_script: rec_au(13),

        upper_limit_gap_min: rec_au(14),
        upper_limit_baseline_rise_min: rec_au(15),
        lower_limit_gap_min: rec_au(16),
        lower_limit_baseline_drop_min: rec_au(17),
        overbar_vertical_gap: rec_au(39),
        overbar_rule_thickness: rec_au(40),
        overbar_extra_ascender: rec_au(41),
        underbar_vertical_gap: rec_au(42),
        underbar_rule_thickness: rec_au(43),
        underbar_extra_descender: rec_au(44),
        stretch_stack_top_shift_up: rec_au(24),
        stretch_stack_bottom_shift_down: rec_au(25),
        stretch_stack_gap_above_min: rec_au(26),
        stretch_stack_gap_below_min: rec_au(27),
    })
}

pub(crate) fn math_glyph_extents(
    skrifa_font: &SkrifaFont,
    ppem: f64,
    ch: char,
) -> Option<MathGlyphExtents> {
    let glyph_id = skrifa_font.charmap().map_unicode(ch)?;
    Some(glyph_extents_by_id(skrifa_font, ppem, glyph_id.to_u32()))
}

pub(crate) fn math_char_italics_correction(
    skrifa_font: &SkrifaFont,
    math_data: Option<&[u8]>,
    ppem: f64,
    ch: char,
) -> Au {
    let units_per_em = skrifa_font.units_per_em();
    if units_per_em == 0 {
        return Au::zero();
    }
    let Some(glyph_id) = skrifa_font.charmap().map_unicode(ch) else {
        return Au::zero();
    };
    let Ok(gid_u16) = u16::try_from(glyph_id.to_u32()) else {
        return Au::zero();
    };
    let Some(view) = math_data.and_then(MathTableView::parse) else {
        return Au::zero();
    };
    view.glyph_italics_correction(gid_u16)
        .map(|v| scale_funit_to_au(v as i32, ppem, units_per_em))
        .unwrap_or_else(Au::zero)
}

pub(crate) fn math_char_top_accent_attachment(
    skrifa_font: &SkrifaFont,
    math_data: Option<&[u8]>,
    ppem: f64,
    ch: char,
) -> Option<Au> {
    let units_per_em = skrifa_font.units_per_em();
    if units_per_em == 0 {
        return None;
    }
    let glyph_id = skrifa_font.charmap().map_unicode(ch)?;
    let gid_u16 = u16::try_from(glyph_id.to_u32()).ok()?;
    let view = MathTableView::parse(math_data?)?;
    let val = view.glyph_top_accent_attachment(gid_u16)?;
    Some(scale_funit_to_au(val as i32, ppem, units_per_em))
}

pub(crate) fn math_stretch_glyph(
    skrifa_font: &SkrifaFont,
    math_data: Option<&[u8]>,
    ppem: f64,
    ch: char,
    direction: MathStretchDirection,
    target_size: Au,
) -> Option<StretchedMathGlyph> {
    let units_per_em = skrifa_font.units_per_em();
    if units_per_em == 0 {
        return None;
    }
    let base_gid = skrifa_font.charmap().map_unicode(ch)?;
    let base_glyph = base_gid.to_u32();

    let view = math_data.and_then(MathTableView::parse);

    let make_single = |glyph_id: u32| -> StretchedMathGlyph {
        let ext = glyph_extents_by_id(skrifa_font, ppem, glyph_id);
        let (ic, accent) = if let (Some(view), Ok(gid_u16)) = (&view, u16::try_from(glyph_id)) {
            let ic = view
                .glyph_italics_correction(gid_u16)
                .map(|v| scale_funit_to_au(v as i32, ppem, units_per_em))
                .unwrap_or_else(Au::zero);
            let accent = view
                .glyph_top_accent_attachment(gid_u16)
                .map(|v| scale_funit_to_au(v as i32, ppem, units_per_em));
            (ic, accent)
        } else {
            (Au::zero(), None)
        };
        StretchedMathGlyph {
            glyphs: vec![PositionedMathGlyph {
                glyph_id,
                x_offset: Au::zero(),
                y_offset: Au::zero(),
            }],
            inline_size: ext.advance.max(Au::zero()),
            ascent: ext.ascent,
            descent: ext.descent,
            italics_correction: ic,
            top_accent_attachment: accent,
        }
    };

    let Some(view) = view.as_ref() else {
        return Some(make_single(base_glyph));
    };
    let Ok(base_gid_u16) = u16::try_from(base_glyph) else {
        return Some(make_single(base_glyph));
    };

    let Some((min_overlap_fu, construction)) = view.glyph_construction(base_gid_u16, direction)
    else {
        return Some(make_single(base_glyph));
    };

    let assembly_offset = read_u16(construction, 0).unwrap_or(0) as usize;
    let variant_count = read_u16(construction, 2).unwrap_or(0) as usize;

    // 1. Check size variants in ascending order.
    let mut last_variant_glyph: Option<u32> = None;
    for i in 0..variant_count {
        let base = 4 + i * 4;
        let Some(variant_gid) = read_u16(construction, base) else {
            break;
        };
        let Some(advance_fu) = read_u16(construction, base + 2) else {
            break;
        };
        last_variant_glyph = Some(variant_gid as u32);
        let advance = scale_funit_to_au(advance_fu as i32, ppem, units_per_em);
        if advance >= target_size {
            return Some(make_single(variant_gid as u32));
        }
    }

    // 2. Build multi-part glyph assembly if variants were not large enough.
    if assembly_offset != 0
        && let Some(assembly) = construction.get(assembly_offset..)
        && let (Some(ic_fu), Some(part_count)) =
            (read_math_value_record(assembly, 0), read_u16(assembly, 4))
    {
        let part_count = part_count as usize;
        let mut parts = Vec::with_capacity(part_count);
        for i in 0..part_count {
            let off = 6 + i * 10;
            let (Some(glyph_id), Some(start_c), Some(end_c), Some(full_adv), Some(flags)) = (
                read_u16(assembly, off),
                read_u16(assembly, off + 2),
                read_u16(assembly, off + 4),
                read_u16(assembly, off + 6),
                read_u16(assembly, off + 8),
            ) else {
                break;
            };
            parts.push(RawGlyphPart {
                glyph_id: glyph_id as u32,
                start_connector_length: start_c,
                end_connector_length: end_c,
                full_advance: full_adv,
                flags,
            });
        }

        if !parts.is_empty() {
            let min_overlap =
                scale_funit_to_au(min_overlap_fu as i32, ppem, units_per_em).max(Au::zero());
            let ic = scale_funit_to_au(ic_fu as i32, ppem, units_per_em);
            if let Some(assembled) = assemble_math_glyph(
                skrifa_font,
                ppem,
                units_per_em,
                &parts,
                direction,
                target_size,
                min_overlap,
                ic,
            ) {
                return Some(assembled);
            }
        }
    }

    // 3. Fallback: largest available variant, or base glyph.
    if let Some(last_gid) = last_variant_glyph {
        return Some(make_single(last_gid));
    }
    Some(make_single(base_glyph))
}

#[expect(clippy::too_many_arguments)]
fn assemble_math_glyph(
    skrifa_font: &SkrifaFont,
    ppem: f64,
    units_per_em: u16,
    parts: &[RawGlyphPart],
    direction: MathStretchDirection,
    target_size: Au,
    min_overlap: Au,
    italics_correction: Au,
) -> Option<StretchedMathGlyph> {
    let is_extender = |p: &RawGlyphPart| (p.flags & EXTENDER_FLAG) != 0;
    let to_au = |v: u16| scale_funit_to_au(v as i32, ppem, units_per_em);

    let non_extender_count = parts.iter().filter(|p| !is_extender(p)).count();
    let extender_count = parts.len() - non_extender_count;

    let build_sequence = |k: usize| -> Vec<&RawGlyphPart> {
        let mut seq = Vec::with_capacity(non_extender_count + extender_count * k);
        for part in parts {
            if is_extender(part) {
                for _ in 0..k {
                    seq.push(part);
                }
            } else {
                seq.push(part);
            }
        }
        seq
    };

    let max_span_for = |seq: &[&RawGlyphPart]| -> Au {
        if seq.is_empty() {
            return Au::zero();
        }
        let mut span = to_au(seq[0].full_advance);
        for pair in seq.windows(2) {
            let max_conn =
                to_au(pair[0].end_connector_length).min(to_au(pair[1].start_connector_length));
            let overlap = min_overlap.min(max_conn).max(Au::zero());
            span += to_au(pair[1].full_advance) - overlap;
        }
        span
    };

    let mut k = if non_extender_count == 0 { 1 } else { 0 };
    let max_k = 256usize;
    let mut seq = build_sequence(k);
    if extender_count > 0 {
        while max_span_for(&seq) < target_size && k < max_k {
            k += 1;
            seq = build_sequence(k);
        }
    }
    if seq.is_empty() {
        return None;
    }

    let num_joints = seq.len().saturating_sub(1);
    let mut overlaps = Vec::with_capacity(num_joints);
    let mut max_extra_overlap = Au::zero();
    for pair in seq.windows(2) {
        let max_conn = to_au(pair[0].end_connector_length)
            .min(to_au(pair[1].start_connector_length))
            .max(Au::zero());
        let base_ov = min_overlap.min(max_conn);
        overlaps.push((base_ov, max_conn));
        max_extra_overlap += max_conn - base_ov;
    }

    let current_span = max_span_for(&seq);
    let excess = (current_span - target_size).max(Au::zero());
    let extra_ratio = if max_extra_overlap > Au::zero() && excess > Au::zero() {
        (excess.0 as f64 / max_extra_overlap.0 as f64).min(1.0)
    } else {
        0.0
    };

    let mut positioned = Vec::with_capacity(seq.len());
    let mut cursor = Au::zero();

    match direction {
        MathStretchDirection::Horizontal => {
            let mut max_ascent = Au(i32::MIN);
            let mut max_descent = Au(i32::MIN);
            for (i, part) in seq.iter().enumerate() {
                if i > 0 {
                    let (base_ov, max_conn) = overlaps[i - 1];
                    let extra = Au(((max_conn - base_ov).0 as f64 * extra_ratio) as i32);
                    cursor -= base_ov + extra;
                }
                let ext = glyph_extents_by_id(skrifa_font, ppem, part.glyph_id);
                max_ascent = max_ascent.max(ext.ascent);
                max_descent = max_descent.max(ext.descent);
                positioned.push(PositionedMathGlyph {
                    glyph_id: part.glyph_id,
                    x_offset: cursor,
                    y_offset: Au::zero(),
                });
                cursor += to_au(part.full_advance);
            }
            Some(StretchedMathGlyph {
                glyphs: positioned,
                inline_size: cursor.max(Au::zero()),
                ascent: if max_ascent == Au(i32::MIN) {
                    Au::zero()
                } else {
                    max_ascent
                },
                descent: if max_descent == Au(i32::MIN) {
                    Au::zero()
                } else {
                    max_descent
                },
                italics_correction,
                top_accent_attachment: None,
            })
        },
        MathStretchDirection::Vertical => {
            // Vertical assemblies in OpenType MATH are ordered bottom-to-top.
            // Place part 0 at y = 0 and step upward (negative y_offset in CSS y-down).
            let mut max_inline = Au::zero();
            let mut max_ascent = Au(i32::MIN);
            let mut max_descent = Au(i32::MIN);
            for (i, part) in seq.iter().enumerate() {
                if i > 0 {
                    let (base_ov, max_conn) = overlaps[i - 1];
                    let extra = Au(((max_conn - base_ov).0 as f64 * extra_ratio) as i32);
                    cursor -= base_ov + extra;
                }
                let ext = glyph_extents_by_id(skrifa_font, ppem, part.glyph_id);
                max_inline = max_inline.max(ext.advance);
                max_ascent = max_ascent.max(cursor + ext.ascent);
                max_descent = max_descent.max(ext.descent - cursor);
                positioned.push(PositionedMathGlyph {
                    glyph_id: part.glyph_id,
                    x_offset: Au::zero(),
                    y_offset: -cursor,
                });
                cursor += to_au(part.full_advance);
            }
            Some(StretchedMathGlyph {
                glyphs: positioned,
                inline_size: max_inline.max(Au::zero()),
                ascent: if max_ascent == Au(i32::MIN) {
                    Au::zero()
                } else {
                    max_ascent
                },
                descent: if max_descent == Au(i32::MIN) {
                    Au::zero()
                } else {
                    max_descent
                },
                italics_correction,
                top_accent_attachment: None,
            })
        },
    }
}
