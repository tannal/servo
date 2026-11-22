/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Underscript (`<munder>`), overscript (`<mover>`), and under-overscript (`<munderover>`) layout.
//!
//! Implements [MathML Core §3.4.4–§3.4.6](https://w3c.github.io/mathml-core/#underscripts-and-overscripts-munder-mover-munderover),
//! including `movablelimits` (display vs. compact style), `accent` / `accentunder`,
//! horizontal (inline-axis) operator stretching, OpenType MATH limit/accent constants,
//! and incremental layout caching.

use app_units::Au;
use fonts::Font;
use layout_api::{LayoutElement, LayoutNode};
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::ServoLayoutNode;
use servo_arc::Arc as ServoArc;
use style::Zero;
use style::context::SharedStyleContext;
use style::logical_geometry::LogicalAxis;
use style::properties::ComputedValues;
use style::values::computed::font::{MathShift, MathStyle};
use web_atoms::{local_name, ns};

use super::mscripts::ScriptParameters;
use super::{MathLevelBox, MathRowBlock};
use crate::cell::ArcRefCell;
use crate::context::LayoutContext;
use crate::dom::WeakLayoutBox;
use crate::dom_traversal::NodeAndStyleInfo;
use crate::layout_box_base::LayoutBoxBase;
use crate::mathml::{
    MathBoxMetrics, MathLayoutCache, MathLayoutResult, MathOperatorProperties, MathStretchAxis,
    MathStretchTarget, translate_fragments,
};
use crate::positioned::PositioningContext;
use crate::sizing::{ContentSizes, InlineContentSizesResult, Size};
use crate::style_ext::LayoutStyle;
use crate::containing_block::{ConstraintSpace, ContainingBlock};

#[derive(Clone, Copy, Debug, PartialEq, Eq, MallocSizeOf)]
pub(crate) enum MathUnderOverKind {
    Under,
    Over,
    UnderOver,
}

#[derive(Clone, Copy, Debug)]
struct UnderOverParameters {
    upper_limit_gap_min: Au,
    upper_limit_baseline_rise_min: Au,
    lower_limit_gap_min: Au,
    lower_limit_baseline_drop_min: Au,
    accent_base_height: Au,
    overbar_vertical_gap: Au,
    overbar_extra_ascender: Au,
    underbar_vertical_gap: Au,
    underbar_extra_descender: Au,
}

impl UnderOverParameters {
    fn new(font: Option<&Font>, font_size: Au, is_display_style: bool) -> Self {
        if let Some(c) = font.and_then(|f| f.math_constants(is_display_style)) {
            return Self {
                upper_limit_gap_min: c.upper_limit_gap_min,
                upper_limit_baseline_rise_min: c.upper_limit_baseline_rise_min,
                lower_limit_gap_min: c.lower_limit_gap_min,
                lower_limit_baseline_drop_min: c.lower_limit_baseline_drop_min,
                accent_base_height: c.accent_base_height,
                overbar_vertical_gap: c.overbar_vertical_gap,
                overbar_extra_ascender: c.overbar_extra_ascender,
                underbar_vertical_gap: c.underbar_vertical_gap,
                underbar_extra_descender: c.underbar_extra_descender,
            };
        }

        let (x_height, rule_thickness) = font
            .map(|f| {
                let m = f.metrics();
                (
                    m.x_height.max(font_size / 2),
                    m.underline_size.max(Au::from_px(1)),
                )
            })
            .unwrap_or((font_size / 2, Au::from_px(1)));

        Self {
            upper_limit_gap_min: rule_thickness * 3,
            upper_limit_baseline_rise_min: Au::zero(),
            lower_limit_gap_min: rule_thickness * 3,
            lower_limit_baseline_drop_min: Au::zero(),
            accent_base_height: x_height,
            overbar_vertical_gap: rule_thickness * 3,
            overbar_extra_ascender: rule_thickness,
            underbar_vertical_gap: rule_thickness * 3,
            underbar_extra_descender: rule_thickness,
        }
    }
}

fn parse_bool_attr<'dom, E: LayoutElement<'dom>>(
    element: Option<E>,
    attr: &web_atoms::LocalName,
) -> Option<bool> {
    let el = element?;
    let val = el.attribute_as_str(&ns!(), attr)?;
    if val.eq_ignore_ascii_case("true") {
        Some(true)
    } else if val.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

/// Box-tree node for `<munder>`, `<mover>`, and `<munderover>`.
#[derive(Debug, MallocSizeOf)]
pub(crate) struct MathUnderOverBlock {
    pub inner_row: MathRowBlock,
    pub kind: MathUnderOverKind,
    pub base_child: Option<ArcRefCell<MathLevelBox>>,
    pub under_child: Option<ArcRefCell<MathLevelBox>>,
    pub over_child: Option<ArcRefCell<MathLevelBox>>,
    pub accent: Option<bool>,
    pub accentunder: Option<bool>,
    layout_cache: MathLayoutCache,
}

impl MathUnderOverBlock {
    pub(crate) fn construct(
        node_and_style_info: &NodeAndStyleInfo,
        children: Vec<ArcRefCell<MathLevelBox>>,
        kind: MathUnderOverKind,
    ) -> Self {
        let mut in_flow = Vec::new();
        let mut out_of_flow = Vec::new();
        for (i, child) in children.iter().enumerate() {
            if child.borrow().is_in_flow() {
                in_flow.push(i);
            } else {
                out_of_flow.push(i);
            }
        }

        let expected = match kind {
            MathUnderOverKind::Under | MathUnderOverKind::Over => 2,
            MathUnderOverKind::UnderOver => 3,
        };

        let element = node_and_style_info.node.as_element();
        let accent = parse_bool_attr(element, &local_name!("accent"));
        let accentunder = parse_bool_attr(element, &local_name!("accentunder"));

        if in_flow.len() != expected {
            return Self {
                inner_row: MathRowBlock::construct(node_and_style_info, children),
                kind,
                base_child: None,
                under_child: None,
                over_child: None,
                accent,
                accentunder,
                layout_cache: MathLayoutCache::default(),
            };
        }

        let mut slots: Vec<Option<ArcRefCell<MathLevelBox>>> =
            children.into_iter().map(Some).collect();
        let take = |slots: &mut [Option<ArcRefCell<MathLevelBox>>], idx: usize| {
            slots[idx].take().unwrap()
        };

        let base_child = Some(take(&mut slots, in_flow[0]));
        let (under_child, over_child) = match kind {
            MathUnderOverKind::Under => (Some(take(&mut slots, in_flow[1])), None),
            MathUnderOverKind::Over => (None, Some(take(&mut slots, in_flow[1]))),
            MathUnderOverKind::UnderOver => (
                Some(take(&mut slots, in_flow[1])),
                Some(take(&mut slots, in_flow[2])),
            ),
        };

        let oof_children: Vec<_> = out_of_flow
            .into_iter()
            .filter_map(|idx| slots[idx].take())
            .collect();

        Self {
            inner_row: MathRowBlock::construct(node_and_style_info, oof_children),
            kind,
            base_child,
            under_child,
            over_child,
            accent,
            accentunder,
            layout_cache: MathLayoutCache::default(),
        }
    }

    pub(crate) fn base(&self) -> &LayoutBoxBase {
        &self.inner_row.base
    }

    pub(crate) fn base_mut(&mut self) -> &mut LayoutBoxBase {
        &mut self.inner_row.base
    }

    pub(crate) fn operator_properties(&self) -> Option<MathOperatorProperties> {
        if let Some(ref base) = self.base_child {
            return base.borrow().operator_properties();
        }
        self.inner_row.operator_properties()
    }

    pub(crate) fn repair_style(
        &mut self,
        context: &SharedStyleContext,
        node: &ServoLayoutNode,
        new_style: &ServoArc<ComputedValues>,
    ) {
        self.inner_row.repair_style(context, node, new_style);
        let element = node.as_element();
        self.accent = parse_bool_attr(element, &local_name!("accent"));
        self.accentunder = parse_bool_attr(element, &local_name!("accentunder"));
        self.layout_cache.clear();
    }

    pub(crate) fn attached_to_tree(&self, layout_box: WeakLayoutBox) {
        if let Some(ref base) = self.base_child {
            base.borrow().attached_to_tree(layout_box.clone());
        }
        if let Some(ref under) = self.under_child {
            under.borrow().attached_to_tree(layout_box.clone());
        }
        if let Some(ref over) = self.over_child {
            over.borrow().attached_to_tree(layout_box.clone());
        }
        self.inner_row.attached_to_tree(layout_box);
    }

    fn should_move_limits(&self) -> bool {
        self.base().style.get_font().math_style == MathStyle::Compact
            && self
                .base_child
                .as_ref()
                .and_then(|b| b.borrow().operator_properties())
                .is_some_and(|p| p.movablelimits)
    }

    pub(crate) fn inline_content_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        self.base().cached_inline_content_sizes_with(constraint_space, || {
            let Some(ref base_child) = self.base_child else {
                return self
                    .inner_row
                    .inline_content_sizes(layout_context, constraint_space);
            };

            let base_s = base_child
                .borrow()
                .inline_content_sizes(layout_context, constraint_space);
            let under_s = self.under_child.as_ref().map(|u| {
                u.borrow()
                    .inline_content_sizes(layout_context, constraint_space)
            });
            let over_s = self.over_child.as_ref().map(|o| {
                o.borrow()
                    .inline_content_sizes(layout_context, constraint_space)
            });

            let (min_w, max_w) = if self.should_move_limits() {
                let script_min = under_s
                    .map_or(Au::zero(), |s| s.sizes.min_content)
                    .max(over_s.map_or(Au::zero(), |s| s.sizes.min_content));
                let script_max = under_s
                    .map_or(Au::zero(), |s| s.sizes.max_content)
                    .max(over_s.map_or(Au::zero(), |s| s.sizes.max_content));
                (
                    base_s.sizes.min_content + script_min,
                    base_s.sizes.max_content + script_max,
                )
            } else {
                (
                    base_s
                        .sizes
                        .min_content
                        .max(under_s.map_or(Au::zero(), |s| s.sizes.min_content))
                        .max(over_s.map_or(Au::zero(), |s| s.sizes.min_content)),
                    base_s
                        .sizes
                        .max_content
                        .max(under_s.map_or(Au::zero(), |s| s.sizes.max_content))
                        .max(over_s.map_or(Au::zero(), |s| s.sizes.max_content)),
                )
            };

            let pbm = LayoutStyle::Default(&self.base().style)
                .content_box_sizes_and_padding_border_margin(&constraint_space.into())
                .pbm;
            let extra =
                pbm.padding_border_sums.inline + pbm.margin.auto_is(Au::zero).inline_sum();

            InlineContentSizesResult {
                sizes: ContentSizes {
                    min_content: min_w + extra,
                    max_content: max_w + extra,
                },
                depends_on_block_constraints: base_s.depends_on_block_constraints
                    || under_s.is_some_and(|s| s.depends_on_block_constraints)
                    || over_s.is_some_and(|s| s.depends_on_block_constraints),
            }
        })
    }

    pub(crate) fn layout(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
        target_size: Option<MathStretchTarget>,
    ) -> MathLayoutResult {
        if let Some(cached) = self.layout_cache.get(
            self.base(),
            containing_block,
            target_size,
            positioning_context,
        ) {
            return cached;
        }

        let Some(ref base_child) = self.base_child else {
            return self.inner_row.layout(
                layout_context,
                positioning_context,
                containing_block,
                target_size,
            );
        };

        let mut local_positioning = PositioningContext::default();
        let style = &self.base().style;
        let is_rtl = !style.writing_mode.is_bidi_ltr();
        let is_display = style.get_font().math_style == MathStyle::Normal;
        let is_cramped = style.get_font().math_shift == MathShift::Compact;
        let font_size = Au::from_f32_px(style.get_font().font_size.computed_size().px());

        let layout_style = LayoutStyle::Default(style);
        let box_sizes =
            layout_style.content_box_sizes_and_padding_border_margin(&containing_block.into());
        let pbm = box_sizes.pbm;

        let font_group = layout_context.font_context.font_group(style.clone_font());
        let font = font_group.first(&layout_context.font_context);

        // If movablelimits is active in compact style, lay out as sub/sup/subsup!
        if self.should_move_limits() {
            let result = self.layout_as_scripts(
                layout_context,
                &mut local_positioning,
                containing_block,
                target_size,
                base_child,
                font.as_ref().map(|f| &***f),
                font_size,
                is_display,
                is_cramped,
                is_rtl,
                &box_sizes,
            );
            self.layout_cache.store(
                self.base(),
                containing_block,
                target_size,
                &result,
                &local_positioning,
            );
            positioning_context.append(local_positioning);
            return result;
        }

        // Standard under/over stacked layout with inline-axis operator stretching.
        let is_inline_stretchy = |child: &ArcRefCell<MathLevelBox>| {
            child.borrow().operator_properties().is_some_and(|p| {
                p.stretchy && p.stretch_axis == MathStretchAxis::Inline
            })
        };

        let base_stretchy = is_inline_stretchy(base_child);
        let under_stretchy = self.under_child.as_ref().is_some_and(is_inline_stretchy);
        let over_stretchy = self.over_child.as_ref().is_some_and(is_inline_stretchy);

        // 1. Measure non-inline-stretchy children first to determine target inline width.
        let mut base_out: Option<MathLayoutResult> = None;
        let mut under_out: Option<MathLayoutResult> = None;
        let mut over_out: Option<MathLayoutResult> = None;
        let mut target_width: Option<Au> = None;

        let incoming_block_stretch = target_size.filter(|t| {
            matches!(t, MathStretchTarget::Block { .. })
                && base_child.borrow().operator_properties().is_some()
        });

        if !base_stretchy {
            let out = base_child.borrow().layout_for_math_parent(
                layout_context,
                &mut local_positioning,
                containing_block,
                style,
                incoming_block_stretch,
            );
            target_width = Some(
                target_width.map_or(out.metrics.size.inline, |w| w.max(out.metrics.size.inline)),
            );
            base_out = Some(out);
        }
        if let Some(ref u) = self.under_child
            && !under_stretchy
        {
            let out = u.borrow().layout_for_math_parent(
                layout_context,
                &mut local_positioning,
                containing_block,
                style,
                None,
            );
            target_width = Some(
                target_width.map_or(out.metrics.size.inline, |w| w.max(out.metrics.size.inline)),
            );
            under_out = Some(out);
        }
        if let Some(ref o) = self.over_child
            && !over_stretchy
        {
            let out = o.borrow().layout_for_math_parent(
                layout_context,
                &mut local_positioning,
                containing_block,
                style,
                None,
            );
            target_width = Some(
                target_width.map_or(out.metrics.size.inline, |w| w.max(out.metrics.size.inline)),
            );
            over_out = Some(out);
        }

        // If all children are inline-stretchy, measure them naturally first.
        if target_width.is_none() {
            let mut max_w = Au::zero();
            for child in [Some(base_child), self.under_child.as_ref(), self.over_child.as_ref()]
                .into_iter()
                .flatten()
            {
                let mut temp_pos = PositioningContext::default();
                let m = child.borrow().layout_for_math_parent(
                    layout_context,
                    &mut temp_pos,
                    containing_block,
                    style,
                    Some(MathStretchTarget::Inline(Au::zero())),
                );
                max_w = max_w.max(m.metrics.size.inline);
            }
            target_width = Some(max_w);
        }

        // Apply incoming inline stretch target if larger.
        let stretch_inline_width = match target_size {
            Some(MathStretchTarget::Inline(req_w)) => target_width.unwrap_or_default().max(req_w),
            _ => target_width.unwrap_or_default(),
        };
        let inline_target = Some(MathStretchTarget::Inline(stretch_inline_width));

        if base_out.is_none() {
            base_out = Some(base_child.borrow().layout_for_math_parent(
                layout_context,
                &mut local_positioning,
                containing_block,
                style,
                inline_target,
            ));
        }
        if let Some(ref u) = self.under_child
            && under_out.is_none()
        {
            under_out = Some(u.borrow().layout_for_math_parent(
                layout_context,
                &mut local_positioning,
                containing_block,
                style,
                inline_target,
            ));
        }
        if let Some(ref o) = self.over_child
            && over_out.is_none()
        {
            over_out = Some(o.borrow().layout_for_math_parent(
                layout_context,
                &mut local_positioning,
                containing_block,
                style,
                inline_target,
            ));
        }

        let mut base_out = base_out.unwrap();
        let params = UnderOverParameters::new(font.as_ref().map(|f| &***f), font_size, is_display);

        let is_accent = self.accent.unwrap_or_else(|| {
            self.over_child
                .as_ref()
                .and_then(|o| o.borrow().operator_properties())
                .is_some_and(|p| p.accent)
        });
        let is_accentunder = self.accentunder.unwrap_or_else(|| {
            self.under_child
                .as_ref()
                .and_then(|u| u.borrow().operator_properties())
                .is_some_and(|p| p.accent)
        });

        let base_ascent = base_out.metrics.ascent();
        let base_descent = base_out.metrics.descent();
        let base_ic = base_out.metrics.italic_correction;

        // 2. Vertical shifts for over and under.
        let (over_shift_up, total_ascent) = if let Some(ref over) = over_out {
            let over_ascent = over.metrics.ascent();
            let over_descent = over.metrics.descent();
            if is_accent {
                let shift = base_ascent.max(params.accent_base_height)
                    + params.overbar_vertical_gap
                    + over_descent;
                let top = shift + over_ascent + params.overbar_extra_ascender;
                (shift, base_ascent.max(top))
            } else {
                let shift = (base_ascent + params.upper_limit_baseline_rise_min)
                    .max(base_ascent + params.upper_limit_gap_min + over_descent);
                let top = shift + over_ascent + params.overbar_extra_ascender;
                (shift, base_ascent.max(top))
            }
        } else {
            (Au::zero(), base_ascent)
        };

        let (under_shift_down, total_descent) = if let Some(ref under) = under_out {
            let under_ascent = under.metrics.ascent();
            let under_descent = under.metrics.descent();
            if is_accentunder {
                let shift = base_descent + params.underbar_vertical_gap + under_ascent;
                let bottom = shift + under_descent + params.underbar_extra_descender;
                (shift, base_descent.max(bottom))
            } else {
                let shift = (base_descent + params.lower_limit_baseline_drop_min)
                    .max(base_descent + params.lower_limit_gap_min + under_ascent);
                let bottom = shift + under_descent + params.underbar_extra_descender;
                (shift, base_descent.max(bottom))
            }
        } else {
            (Au::zero(), base_descent)
        };

        // 3. Inline width and horizontal centering (with italic correction / top accent attachment).
        let base_w = base_out.metrics.size.inline;
        let under_w = under_out
            .as_ref()
            .map_or(Au::zero(), |u| u.metrics.size.inline);
        let over_w = over_out
            .as_ref()
            .map_or(Au::zero(), |o| o.metrics.size.inline);
        let natural_width = base_w.max(under_w).max(over_w);

        let content_width = box_sizes.content_box_sizes.inline.resolve(
            LogicalAxis::Inline,
            Size::FitContent,
            Au::zero,
            Some(containing_block.inline_size()),
            || natural_width.into(),
            false,
        );
        let natural_block = total_ascent + total_descent;
        let content_block = box_sizes.content_box_sizes.block.resolve(
            LogicalAxis::Block,
            Size::FitContent,
            Au::zero,
            containing_block.block_size(),
            || natural_block.into(),
            false,
        );

        let base_x = (content_width - base_w) / 2;
        let base_y = total_ascent - base_ascent;
        translate_fragments(&mut base_out.fragments, base_x, base_y);

        let mut children = Vec::new();
        children.extend(base_out.fragments);

        if let Some(mut over) = over_out {
            let over_x = if is_accent
                && let (Some(base_attach), Some(over_attach)) = (
                    base_out.metrics.top_accent_attachment,
                    over.metrics.top_accent_attachment,
                )
            {
                base_x + base_attach - over_attach
            } else {
                let ic_shift = if is_accent { Au::zero() } else { base_ic / 2 };
                (content_width - over_w) / 2 + if is_rtl { -ic_shift } else { ic_shift }
            };
            let over_y = total_ascent - over_shift_up - over.metrics.ascent();
            translate_fragments(&mut over.fragments, over_x, over_y);
            children.extend(over.fragments);
        }

        if let Some(mut under) = under_out {
            let ic_shift = if is_accentunder {
                Au::zero()
            } else {
                base_ic / 2
            };
            let under_x = (content_width - under_w) / 2 - if is_rtl { -ic_shift } else { ic_shift };
            let under_y = total_ascent + under_shift_down - under.metrics.ascent();
            translate_fragments(&mut under.fragments, under_x, under_y);
            children.extend(under.fragments);
        }

        for oof_child in &self.inner_row.children {
            let oof_out = oof_child.borrow().layout_for_math_parent(
                layout_context,
                &mut local_positioning,
                containing_block,
                style,
                None,
            );
            children.extend(oof_out.fragments);
        }

        let result = MathLayoutResult::from_content(
            self.base(),
            containing_block,
            &pbm,
            children,
            MathBoxMetrics::new(content_width, content_block, total_ascent),
            None,
        );
        self.layout_cache.store(
            self.base(),
            containing_block,
            target_size,
            &result,
            &local_positioning,
        );
        positioning_context.append(local_positioning);
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn layout_as_scripts(
        &self,
        layout_context: &LayoutContext,
        local_positioning: &mut PositioningContext,
        containing_block: &ContainingBlock,
        target_size: Option<MathStretchTarget>,
        base_child: &ArcRefCell<MathLevelBox>,
        font: Option<&Font>,
        font_size: Au,
        is_display: bool,
        is_cramped: bool,
        is_rtl: bool,
        box_sizes: &crate::style_ext::ContentBoxSizesAndPBM,
    ) -> MathLayoutResult {
        let style = &self.base().style;
        let base_borrow = base_child.borrow();
        let base_stretch = target_size.filter(|_| base_borrow.operator_properties().is_some());
        let mut base_out = base_borrow.layout_for_math_parent(
            layout_context,
            local_positioning,
            containing_block,
            style,
            base_stretch,
        );
        let mut sub_out = self.under_child.as_ref().map(|u| {
            u.borrow().layout_for_math_parent(
                layout_context,
                local_positioning,
                containing_block,
                style,
                None,
            )
        });
        let mut sup_out = self.over_child.as_ref().map(|o| {
            o.borrow().layout_for_math_parent(
                layout_context,
                local_positioning,
                containing_block,
                style,
                None,
            )
        });

        let params = ScriptParameters::new(font, font_size, is_display, is_cramped);
        let base_ascent = base_out.metrics.ascent();
        let base_descent = base_out.metrics.descent();
        let base_ic = base_out.metrics.italic_correction;

        let (sub_shift, sup_shift) = params.compute_shifts(
            base_ascent,
            base_descent,
            sub_out.as_ref().map(|o| &o.metrics),
            sup_out.as_ref().map(|o| &o.metrics),
        );

        let mut total_ascent = base_ascent;
        let mut total_descent = base_descent;
        if let Some(ref sub) = sub_out {
            total_ascent = total_ascent.max(sub.metrics.ascent() - sub_shift);
            total_descent = total_descent.max(sub_shift + sub.metrics.descent());
        }
        if let Some(ref sup) = sup_out {
            total_ascent = total_ascent.max(sup_shift + sup.metrics.ascent());
            total_descent = total_descent.max(sup.metrics.descent() - sup_shift);
        }

        let sub_w = sub_out
            .as_ref()
            .map_or(Au::zero(), |o| (o.metrics.size.inline - base_ic).max(Au::zero()));
        let sup_w = sup_out
            .as_ref()
            .map_or(Au::zero(), |o| o.metrics.size.inline);
        let script_w = sub_w.max(sup_w);
        let base_w = base_out.metrics.size.inline;
        let natural_width = base_w + script_w + params.space_after_script;
        let content_width = box_sizes.content_box_sizes.inline.resolve(
            LogicalAxis::Inline,
            Size::FitContent,
            Au::zero,
            Some(containing_block.inline_size()),
            || natural_width.into(),
            false,
        );
        let natural_block = total_ascent + total_descent;
        let content_block = box_sizes.content_box_sizes.block.resolve(
            LogicalAxis::Block,
            Size::FitContent,
            Au::zero,
            containing_block.block_size(),
            || natural_block.into(),
            false,
        );

        let (base_x, script_x) = if is_rtl {
            (params.space_after_script + script_w, params.space_after_script)
        } else {
            (Au::zero(), base_w)
        };

        let mut children = Vec::new();
        translate_fragments(&mut base_out.fragments, base_x, total_ascent - base_ascent);
        children.extend(base_out.fragments);

        if let Some(mut sub) = sub_out.take() {
            let x = if is_rtl {
                script_x + script_w - sub.metrics.size.inline + base_ic
            } else {
                script_x - base_ic
            };
            let y = total_ascent + sub_shift - sub.metrics.ascent();
            translate_fragments(&mut sub.fragments, x, y);
            children.extend(sub.fragments);
        }
        if let Some(mut sup) = sup_out.take() {
            let x = if is_rtl {
                script_x + script_w - sup.metrics.size.inline
            } else {
                script_x
            };
            let y = total_ascent - sup_shift - sup.metrics.ascent();
            translate_fragments(&mut sup.fragments, x, y);
            children.extend(sup.fragments);
        }

        MathLayoutResult::from_content(
            self.base(),
            containing_block,
            &box_sizes.pbm,
            children,
            MathBoxMetrics::new(content_width, content_block, total_ascent),
            None,
        )
    }
}
