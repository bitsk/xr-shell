// SPDX-License-Identifier: GPL-3.0-or-later
//! CPU scene rasterisation is an explicit M2 baseline. Vulkan composites the
//! resulting planes and application snapshots into both real XR swapchains.
use anyhow::{Result, ensure};
use std::ffi::{c_char, c_double, c_int, c_void};
use xr_shell_protocols::Node;
use xr_spatial_kit::{TextStyle, Theme};
type Ptr = *mut c_void;
#[repr(C)]
#[derive(Default)]
struct TextRectangle {
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
}
#[link(name = "libcairo.so.2", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {
    fn cairo_image_surface_create(format: c_int, width: c_int, height: c_int) -> Ptr;
    fn cairo_image_surface_create_for_data(
        data: *mut u8,
        format: c_int,
        width: c_int,
        height: c_int,
        stride: c_int,
    ) -> Ptr;
    fn cairo_image_surface_get_data(surface: Ptr) -> *mut u8;
    fn cairo_surface_flush(surface: Ptr);
    fn cairo_surface_destroy(surface: Ptr);
    fn cairo_surface_status(surface: Ptr) -> c_int;
    fn cairo_create(surface: Ptr) -> Ptr;
    fn cairo_destroy(cr: Ptr);
    fn cairo_set_source_rgba(cr: Ptr, r: c_double, g: c_double, b: c_double, a: c_double);
    fn cairo_rectangle(cr: Ptr, x: c_double, y: c_double, w: c_double, h: c_double);
    fn cairo_fill(cr: Ptr);
    fn cairo_paint(cr: Ptr);
    fn cairo_paint_with_alpha(cr: Ptr, alpha: c_double);
    fn cairo_move_to(cr: Ptr, x: c_double, y: c_double);
    fn cairo_close_path(cr: Ptr);
    fn cairo_new_path(cr: Ptr);
    fn cairo_line_to(cr: Ptr, x: c_double, y: c_double);
    fn cairo_curve_to(
        cr: Ptr,
        x1: c_double,
        y1: c_double,
        x2: c_double,
        y2: c_double,
        x3: c_double,
        y3: c_double,
    );
    fn cairo_set_line_width(cr: Ptr, width: c_double);
    fn cairo_set_line_cap(cr: Ptr, cap: c_int);
    fn cairo_stroke(cr: Ptr);
    fn cairo_pattern_create_linear(x0: c_double, y0: c_double, x1: c_double, y1: c_double) -> Ptr;
    fn cairo_pattern_add_color_stop_rgba(
        pattern: Ptr,
        offset: c_double,
        r: c_double,
        g: c_double,
        b: c_double,
        a: c_double,
    );
    fn cairo_pattern_destroy(pattern: Ptr);
    fn cairo_set_source(cr: Ptr, source: Ptr);
    fn cairo_arc(cr: Ptr, x: c_double, y: c_double, r: c_double, a1: c_double, a2: c_double);
    fn cairo_save(cr: Ptr);
    fn cairo_restore(cr: Ptr);
    fn cairo_clip(cr: Ptr);
    fn cairo_scale(cr: Ptr, x: c_double, y: c_double);
    fn cairo_translate(cr: Ptr, x: c_double, y: c_double);
    fn cairo_set_source_surface(cr: Ptr, surface: Ptr, x: c_double, y: c_double);
}
#[link(
    name = "libpangocairo-1.0.so.0",
    kind = "dylib",
    modifiers = "+verbatim"
)]
unsafe extern "C" {
    fn pango_cairo_create_layout(cr: Ptr) -> Ptr;
    fn pango_cairo_show_layout(cr: Ptr, layout: Ptr);
}
#[link(name = "libpango-1.0.so.0", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {
    fn pango_layout_set_text(layout: Ptr, text: *const c_char, length: c_int);
    fn pango_layout_set_width(layout: Ptr, width: c_int);
    fn pango_layout_set_alignment(layout: Ptr, alignment: c_int);
    fn pango_layout_get_pixel_extents(
        layout: Ptr,
        ink: *mut TextRectangle,
        logical: *mut TextRectangle,
    );
    fn pango_layout_set_single_paragraph_mode(layout: Ptr, single: c_int);
    fn pango_layout_set_attributes(layout: Ptr, attrs: Ptr);
    fn pango_attr_list_new() -> Ptr;
    fn pango_attr_font_features_new(features: *const c_char) -> Ptr;
    fn pango_attr_list_insert(list: Ptr, attr: Ptr);
    fn pango_attr_list_unref(list: Ptr);
    fn pango_font_description_set_weight(desc: Ptr, weight: c_int);
    fn pango_layout_set_height(layout: Ptr, height: c_int);
    fn pango_layout_set_ellipsize(layout: Ptr, mode: c_int);
    fn pango_font_description_from_string(text: *const c_char) -> Ptr;
    fn pango_font_description_set_absolute_size(desc: Ptr, size: c_double);
    fn pango_layout_set_font_description(layout: Ptr, desc: Ptr);
    fn pango_font_description_free(desc: Ptr);
}
#[link(name = "libgobject-2.0.so.0", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {
    fn g_object_unref(object: Ptr);
}

pub struct Canvas {
    surface: Ptr,
    cr: Ptr,
    pub width: u32,
    pub height: u32,
}
impl Canvas {
    pub fn for_logical(w: u32, h: u32) -> Result<Self> {
        ensure!(w > 0 && h > 0, "invalid logical canvas size");
        let scale = (2048. / w.max(h) as f64).min(2.);
        let canvas = Self::new(
            (w as f64 * scale).round().max(1.) as u32,
            (h as f64 * scale).round().max(1.) as u32,
        )?;
        unsafe {
            cairo_scale(
                canvas.cr,
                canvas.width as f64 / w as f64,
                canvas.height as f64 / h as f64,
            );
        }
        Ok(canvas)
    }
    pub fn new(w: u32, h: u32) -> Result<Self> {
        ensure!(
            w > 0 && h > 0 && w <= 2048 && h <= 2048,
            "invalid canvas size"
        );
        unsafe {
            let surface = cairo_image_surface_create(0, w as _, h as _);
            ensure!(
                cairo_surface_status(surface) == 0,
                "Cairo allocation failed"
            );
            let cr = cairo_create(surface);
            Ok(Self {
                surface,
                cr,
                width: w,
                height: h,
            })
        }
    }
    pub fn rounded(&self, rect: [f32; 4], colour: [f32; 4], radius: f32) {
        unsafe {
            let [x, y, w, h] = rect.map(|n| n as f64);
            let r = (radius as f64).min(w / 2.).min(h / 2.).max(0.);
            let c = self.cr;
            cairo_set_source_rgba(
                c,
                colour[0] as _,
                colour[1] as _,
                colour[2] as _,
                colour[3] as _,
            );
            cairo_move_to(c, x + r, y);
            cairo_arc(c, x + w - r, y + r, r, -std::f64::consts::FRAC_PI_2, 0.);
            cairo_arc(c, x + w - r, y + h - r, r, 0., std::f64::consts::FRAC_PI_2);
            cairo_arc(
                c,
                x + r,
                y + h - r,
                r,
                std::f64::consts::FRAC_PI_2,
                std::f64::consts::PI,
            );
            cairo_arc(
                c,
                x + r,
                y + r,
                r,
                std::f64::consts::PI,
                3. * std::f64::consts::FRAC_PI_2,
            );
            cairo_close_path(c);
            cairo_fill(c);
        }
    }
    pub fn text(&self, rect: [f32; 4], text: &str, colour: [f32; 4], size: f32) {
        self.text_styled(rect, text, colour, size, 400, false);
    }
    pub fn text_styled(
        &self,
        rect: [f32; 4],
        text: &str,
        colour: [f32; 4],
        size: f32,
        weight: i32,
        center: bool,
    ) {
        self.layout_text(
            rect,
            text,
            colour,
            size,
            weight,
            center,
            "Noto Sans CJK SC, Noto Sans, sans-serif",
            false,
        );
    }
    pub fn text_token(
        &self,
        rect: [f32; 4],
        text: &str,
        colour: [f32; 4],
        theme: &Theme,
        name: &str,
        center: bool,
    ) {
        let fallback = TextStyle {
            size: theme.font_size,
            weight: 500,
            tabular_numbers: false,
        };
        let style = theme.typography.get(name).unwrap_or(&fallback);
        self.layout_text(
            rect,
            text,
            colour,
            style.size,
            style.weight as i32,
            center,
            &theme.font_families.join(", "),
            style.tabular_numbers,
        );
    }
    fn layout_text(
        &self,
        rect: [f32; 4],
        text: &str,
        colour: [f32; 4],
        size: f32,
        weight: i32,
        center: bool,
        family: &str,
        tabular: bool,
    ) {
        unsafe {
            let text = text.replace('\0', " ");
            let c = self.cr;
            let layout = pango_cairo_create_layout(c);
            let family = std::ffi::CString::new(family.replace('\0', " ")).unwrap();
            let desc = pango_font_description_from_string(family.as_ptr());
            pango_font_description_set_absolute_size(desc, size as f64 * 1024.);
            pango_font_description_set_weight(desc, weight);
            pango_layout_set_font_description(layout, desc);
            pango_layout_set_alignment(layout, if center { 1 } else { 0 });
            pango_layout_set_text(layout, text.as_ptr() as _, text.len() as _);
            pango_layout_set_width(layout, (rect[2] * 1024.) as _);
            pango_layout_set_single_paragraph_mode(layout, 1);
            pango_layout_set_height(layout, -1);
            pango_layout_set_ellipsize(layout, 3);
            if tabular {
                let attrs = pango_attr_list_new();
                pango_attr_list_insert(
                    attrs,
                    pango_attr_font_features_new(c"tnum=1,lnum=1".as_ptr()),
                );
                pango_layout_set_attributes(layout, attrs);
                pango_attr_list_unref(attrs);
            }
            let mut logical = TextRectangle::default();
            pango_layout_get_pixel_extents(layout, std::ptr::null_mut(), &mut logical);
            cairo_save(c);
            cairo_rectangle(c, rect[0] as _, rect[1] as _, rect[2] as _, rect[3] as _);
            cairo_clip(c);
            cairo_move_to(
                c,
                rect[0] as _,
                (rect[1] + (rect[3] - logical.height as f32) / 2. - logical.y as f32) as _,
            );
            cairo_set_source_rgba(
                c,
                colour[0] as _,
                colour[1] as _,
                colour[2] as _,
                colour[3] as _,
            );
            pango_cairo_show_layout(c, layout);
            cairo_restore(c);
            pango_font_description_free(desc);
            g_object_unref(layout);
        }
    }
    pub fn image(&self, rect: [f32; 4], width: u32, height: u32, bgra: &[u8]) {
        if bgra.len() != width as usize * height as usize * 4 {
            return;
        }
        unsafe {
            let image = cairo_image_surface_create_for_data(
                bgra.as_ptr() as *mut _,
                0,
                width as _,
                height as _,
                (width * 4) as _,
            );
            cairo_save(self.cr);
            cairo_translate(self.cr, rect[0] as _, rect[1] as _);
            cairo_scale(
                self.cr,
                (rect[2] / width as f32) as _,
                (rect[3] / height as f32) as _,
            );
            cairo_rectangle(self.cr, 0., 0., width as _, height as _);
            cairo_clip(self.cr);
            cairo_set_source_surface(self.cr, image, 0., 0.);
            cairo_paint(self.cr);
            cairo_restore(self.cr);
            cairo_surface_destroy(image);
        }
    }
    /// Stretch only the texture's middle; optical corners keep a fixed size.
    pub fn skin(&self, rect: [f32; 4], name: &str, radius: f32, opacity: f32) -> bool {
        let Some(bitmap) = crate::art::material(name) else {
            return false;
        };
        self.bitmap(rect, bitmap, Some(radius), opacity);
        true
    }
    fn bitmap(
        &self,
        rect: [f32; 4],
        bitmap: &crate::art::Bitmap,
        radius: Option<f32>,
        opacity: f32,
    ) {
        let [x, y, w, h] = rect;
        if w <= 0. || h <= 0. || opacity <= 0. {
            return;
        }
        let [l, t, r, b] = bitmap.insets.map(|v| v as f32);
        let (sx, sy, dx, dy) = if let Some(radius) = radius {
            let cap = radius.min(w / 2.).min(h / 2.).max(0.);
            (
                vec![0., l, bitmap.width as f32 - r, bitmap.width as f32],
                vec![0., t, bitmap.height as f32 - b, bitmap.height as f32],
                vec![x, x + cap, x + w - cap, x + w],
                vec![y, y + cap, y + h - cap, y + h],
            )
        } else {
            (
                vec![0., bitmap.width as f32],
                vec![0., bitmap.height as f32],
                vec![x, x + w],
                vec![y, y + h],
            )
        };
        unsafe {
            let image = cairo_image_surface_create_for_data(
                bitmap.bgra.as_ptr() as *mut _,
                0,
                bitmap.width as _,
                bitmap.height as _,
                (bitmap.width * 4) as _,
            );
            if cairo_surface_status(image) == 0 {
                for row in 0..sy.len() - 1 {
                    for col in 0..sx.len() - 1 {
                        let sw = sx[col + 1] - sx[col];
                        let sh = sy[row + 1] - sy[row];
                        let dw = dx[col + 1] - dx[col];
                        let dh = dy[row + 1] - dy[row];
                        if sw <= 0. || sh <= 0. || dw <= 0. || dh <= 0. {
                            continue;
                        }
                        cairo_save(self.cr);
                        cairo_translate(self.cr, dx[col] as _, dy[row] as _);
                        cairo_scale(self.cr, (dw / sw) as _, (dh / sh) as _);
                        cairo_rectangle(self.cr, 0., 0., sw as _, sh as _);
                        cairo_clip(self.cr);
                        cairo_set_source_surface(self.cr, image, -sx[col] as f64, -sy[row] as f64);
                        cairo_paint_with_alpha(self.cr, opacity.clamp(0., 1.) as _);
                        cairo_restore(self.cr);
                    }
                }
            }
            cairo_surface_destroy(image);
        }
    }
    fn rounded_path(&self, rect: [f32; 4], radius: f32) {
        let [x, y, w, h] = rect.map(|n| n as f64);
        let r = (radius as f64).min(w / 2.).min(h / 2.).max(0.);
        unsafe {
            cairo_new_path(self.cr);
            cairo_move_to(self.cr, x + r, y);
            cairo_arc(
                self.cr,
                x + w - r,
                y + r,
                r,
                -std::f64::consts::FRAC_PI_2,
                0.,
            );
            cairo_arc(
                self.cr,
                x + w - r,
                y + h - r,
                r,
                0.,
                std::f64::consts::FRAC_PI_2,
            );
            cairo_arc(
                self.cr,
                x + r,
                y + h - r,
                r,
                std::f64::consts::FRAC_PI_2,
                std::f64::consts::PI,
            );
            cairo_arc(
                self.cr,
                x + r,
                y + r,
                r,
                std::f64::consts::PI,
                3. * std::f64::consts::FRAC_PI_2,
            );
            cairo_close_path(self.cr);
        }
    }
    pub fn rounded_image(&self, rect: [f32; 4], w: u32, h: u32, pixels: &[u8], radius: f32) {
        unsafe {
            cairo_save(self.cr);
        }
        self.rounded_path(rect, radius);
        unsafe {
            cairo_clip(self.cr);
        }
        self.image(rect, w, h, pixels);
        unsafe {
            cairo_restore(self.cr);
        }
    }
    pub fn gradient_pill(&self, rect: [f32; 4], left: [f32; 4], right: [f32; 4], radius: f32) {
        if rect[2] <= 0. {
            return;
        }
        unsafe {
            cairo_save(self.cr);
            self.rounded_path(rect, radius);
            cairo_clip(self.cr);
            let p = cairo_pattern_create_linear(rect[0] as _, 0., (rect[0] + rect[2]) as _, 0.);
            for (offset, c) in [(0., left), (1., right)] {
                cairo_pattern_add_color_stop_rgba(
                    p, offset, c[0] as _, c[1] as _, c[2] as _, c[3] as _,
                );
            }
            cairo_set_source(self.cr, p);
            cairo_paint(self.cr);
            cairo_pattern_destroy(p);
            cairo_restore(self.cr);
        }
    }
    pub fn speaker(&self, x: f32, y: f32) {
        unsafe {
            let c = self.cr;
            let (x, y) = (x as f64, y as f64);
            cairo_save(c);
            cairo_set_source_rgba(c, 1., 1., 1., 1.);
            cairo_move_to(c, x + 2., y + 7.);
            cairo_line_to(c, x + 6., y + 7.);
            cairo_line_to(c, x + 12., y + 3.);
            cairo_line_to(c, x + 12., y + 19.);
            cairo_line_to(c, x + 6., y + 15.);
            cairo_line_to(c, x + 2., y + 15.);
            cairo_close_path(c);
            cairo_fill(c);
            cairo_set_line_width(c, 1.7);
            for r in [4., 7.] {
                cairo_new_path(c);
                cairo_arc(c, x + 12., y + 11., r, -0.85, 0.85);
                cairo_stroke(c);
            }
            cairo_restore(c);
        }
    }
    pub fn resize_handle(&self, left: bool, hover: bool) {
        unsafe {
            let c = self.cr;
            cairo_save(c);
            cairo_set_line_cap(c, 1);
            for (width, alpha) in [(7., 0.06), (4., 0.14), (2., if hover { 1. } else { 0.65 })] {
                cairo_new_path(c);
                let x = if left { 4. } else { 20. };
                let sign = if left { 1. } else { -1. };
                cairo_move_to(c, x, 2.);
                cairo_line_to(c, x, 14.);
                cairo_curve_to(c, x, 19., x, 20., x + sign * 6., 20.);
                cairo_line_to(c, x + sign * 18., 20.);
                cairo_set_line_width(c, width);
                cairo_set_source_rgba(
                    c,
                    if hover { 0.30 } else { 1. },
                    if hover { 0.64 } else { 1. },
                    1.,
                    alpha,
                );
                cairo_stroke(c);
            }
            cairo_restore(c);
        }
    }
    pub fn rgba(&self) -> Vec<u8> {
        unsafe {
            cairo_surface_flush(self.surface);
            let bytes = std::slice::from_raw_parts(
                cairo_image_surface_get_data(self.surface),
                self.width as usize * self.height as usize * 4,
            );
            let mut out = Vec::with_capacity(bytes.len());
            for p in bytes.chunks_exact(4) {
                let a = p[3] as u32;
                for v in [p[2], p[1], p[0]] {
                    out.push(if a == 0 {
                        0
                    } else {
                        ((v as u32 * 255 + a / 2) / a).min(255) as u8
                    });
                }
                out.push(p[3]);
            }
            out
        }
    }
}
impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            cairo_destroy(self.cr);
            cairo_surface_destroy(self.surface);
        }
    }
}

pub fn paint_node(
    canvas: &Canvas,
    node: &Node,
    rect: [f32; 4],
    theme: &Theme,
    pressed: bool,
    hovered: bool,
    spatial: bool,
) {
    let [x, y, w, h] = rect;
    let primary = theme.colour("text.primary");
    let secondary = theme.colour("text.secondary");
    let ink = if node.enabled { primary } else { secondary };
    match node.kind.as_str() {
        "Group" => {}
        "Card" => {
            if spatial {
                return;
            }
            let capsule = node.style_id == "dock.background";
            let radius = if capsule { h / 2. } else { 20. };
            canvas.rounded(rect, theme.colour(&node.style_id), radius);
            if capsule || node.style_id == "panel.background" {
                canvas.skin(
                    rect,
                    if capsule {
                        "capsule-normal"
                    } else {
                        "panel-normal"
                    },
                    radius,
                    if capsule { 0.32 } else { 0.4 },
                );
            }
        }
        "Button" => {
            let active = ["button.primary", "dock.active", "dock.active-icon"]
                .contains(&node.style_id.as_str());
            let app_icon = ["dock.app-icon", "dock.active-icon"].contains(&node.style_id.as_str());
            let status_icon = node.style_id == "dock.status-icon";
            let colour = if pressed && !active {
                theme.colour("pressed")
            } else if hovered && !active {
                theme.colour("hover")
            } else {
                theme.colour(&node.style_id)
            };
            let radius = if app_icon || status_icon {
                12.
            } else if node.style_id.starts_with("dock.") {
                h / 2.
            } else if node.style_id == "keyboard.key" {
                12.
            } else {
                12.
            };
            if !spatial {
                canvas.rounded(rect, colour, radius);
            }
            let key = node.style_id == "keyboard.key";
            if !spatial && !app_icon && !status_icon && w >= 24. && h >= 24. {
                let skin = match (key, active, pressed, hovered) {
                    (true, true, _, _) => "key-selected",
                    (true, _, true, _) => "key-pressed",
                    (true, _, _, true) => "key-hover",
                    (true, _, _, _) => "key-normal",
                    (_, true, _, _) => "panel-selected",
                    (_, _, true, _) => "panel-pressed",
                    (_, _, _, true) => "panel-hover",
                    _ => "panel-normal",
                };
                canvas.skin(rect, skin, radius, if active { 0.12 } else { 0.35 });
            }
            let has_icon = !node.icon_id.is_empty();
            let icon_size = (h - 8.)
                .min(if app_icon {
                    48.
                } else if status_icon {
                    36.
                } else {
                    32.
                })
                .max(16.);
            let icon_x = if node.text.is_empty() {
                x + (w - icon_size) / 2.
            } else {
                x + 10.
            };
            if has_icon
                && !canvas.icon(
                    [icon_x, y + (h - icon_size) / 2., icon_size, icon_size],
                    &node.icon_id,
                )
            {
                canvas.text([icon_x, y + (h - 26.) / 2., icon_size, 28.], "◈", ink, 24.);
            }
            let left = if has_icon && !node.text.is_empty() {
                icon_size + 18.
            } else {
                10.
            };
            if !node.text.is_empty() {
                canvas.text_token(
                    if key {
                        [x + 4., y + (h - 31.).max(0.) * 0.30, w - 8., h.min(36.)]
                    } else {
                        [x + left, y, (w - left - 8.).max(1.), h]
                    },
                    &node.text,
                    if active { [1.; 4] } else { ink },
                    theme,
                    if key { "key" } else { "button" },
                    key,
                );
            }
        }
        "Slider" => {
            if spatial {
                canvas.speaker(x + 7., y + (h - 22.) / 2.);
                return;
            }
            let track = [x, y + (h - 22.) / 2., w, 22.];
            canvas.rounded(track, [0.75, 0.82, 0.92, 1.], 11.);
            let value = ((node.value - node.min) / (node.max - node.min).max(0.0001)).clamp(0., 1.);
            canvas.gradient_pill(
                [track[0], track[1], w * value, 22.],
                [43. / 255., 125. / 255., 233. / 255., 1.],
                [76. / 255., 161. / 255., 1., 1.],
                11.,
            );
            canvas.speaker(x + 7., track[1]);
            let cx = (x + w * value).clamp(x + 14., (x + w - 14.).max(x + 14.));
            canvas.rounded(
                [cx - 15., y + (h - 28.) / 2. + 2., 30., 28.],
                [0.14, 0.22, 0.36, 0.18],
                14.,
            );
            canvas.rounded(
                [cx - 14., y + (h - 28.) / 2., 28., 28.],
                [0.98, 0.99, 1., 1.],
                14.,
            );
        }
        "Text" => {
            let token = match node.style_id.as_str() {
                s if s.starts_with("taskbar.") => s,
                _ if node.node_id.contains("title") => "title",
                _ => "body",
            };
            canvas.text_token(
                rect,
                &node.text,
                if ["text.secondary", "taskbar.caption"].contains(&node.style_id.as_str()) {
                    secondary
                } else {
                    primary
                },
                theme,
                token,
                false,
            );
        }
        "Icon" => {
            if !canvas.icon(rect, &node.icon_id) {
                canvas.text(rect, "◈", primary, 24.);
            }
        }
        _ => {}
    }
}

#[repr(C)]
struct SvgRectangle {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}
#[link(name = "librsvg-2.so.2", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {
    fn rsvg_handle_new_from_file(file: *const c_char, error: *mut Ptr) -> Ptr;
    fn rsvg_handle_render_document(
        handle: Ptr,
        cr: Ptr,
        viewport: *const SvgRectangle,
        error: *mut Ptr,
    ) -> i32;
    fn rsvg_handle_set_stylesheet(
        handle: Ptr,
        css: *const u8,
        length: usize,
        error: *mut Ptr,
    ) -> i32;
    fn cairo_image_surface_create_from_png(file: *const c_char) -> Ptr;
}
fn icon_path(name: &str) -> Option<std::path::PathBuf> {
    use std::{
        collections::BTreeMap,
        path::PathBuf,
        sync::{Mutex, OnceLock},
    };
    static CACHE: OnceLock<Mutex<BTreeMap<String, Option<PathBuf>>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .ok()?;
    if let Some(path) = cache.get(name) {
        return path.clone();
    }
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.+".contains(&c))
    {
        return None;
    }
    let base = std::path::Path::new("/usr/share/icons");
    let mut themes = Vec::new();
    if let Ok(theme) = std::env::var("XR_ICON_THEME") {
        if !theme.contains('/') {
            themes.push(theme);
        }
    }
    if let Ok(dirs) = std::fs::read_dir(base) {
        for item in dirs.flatten() {
            let n = item.file_name().to_string_lossy().into_owned();
            if n.to_ascii_lowercase().contains("ukui") || n.to_ascii_lowercase().contains("kylin") {
                themes.push(n);
            }
        }
    }
    themes.extend(
        ["hicolor", "Adwaita", "breeze"]
            .iter()
            .map(|s| s.to_string()),
    );
    let mut found = None;
    'search: for theme in themes {
        for size in [
            "scalable", "256x256", "128x128", "64x64", "48x48", "32x32", "symbolic",
        ] {
            for context in [
                "apps",
                "places",
                "actions",
                "status",
                "devices",
                "categories",
                "mimetypes",
            ] {
                for extension in ["svg", "png"] {
                    for id in [name.to_string(), format!("{name}-symbolic")] {
                        let p = base
                            .join(&theme)
                            .join(size)
                            .join(context)
                            .join(format!("{id}.{extension}"));
                        if p.is_file() {
                            found = Some(p);
                            break 'search;
                        }
                    }
                }
            }
        }
    }
    if found.is_none() {
        for ext in ["svg", "png"] {
            let p = std::path::Path::new("/usr/share/pixmaps").join(format!("{name}.{ext}"));
            if p.is_file() {
                found = Some(p);
                break;
            }
        }
    }
    if let Some(path) = &found {
        println!("XR_ICON_RESOURCE id={name} path={}", path.display());
    }
    cache.insert(name.into(), found.clone());
    found
}
impl Canvas {
    fn battery_symbol(&self, rect: [f32; 4], name: &str, ink: [f32; 4]) -> bool {
        let (level, badge) = name.split_once('-').unwrap_or((name, ""));
        let level = if level == "unknown" {
            None
        } else {
            let Ok(level @ 0..=100) = level.parse::<u8>() else {
                return false;
            };
            Some(level)
        };
        if !matches!(badge, "" | "charging" | "fast-charging" | "plugged") {
            return false;
        }
        unsafe {
            let c = self.cr;
            cairo_save(c);
            cairo_translate(c, rect[0] as _, rect[1] as _);
            cairo_scale(c, rect[2] as f64 / 44., rect[3] as f64 / 32.);
            cairo_set_source_rgba(c, ink[0] as _, ink[1] as _, ink[2] as _, ink[3] as _);
            cairo_set_line_width(c, 2.);
            cairo_set_line_cap(c, 1);
            cairo_new_path(c);

            // Keep the level fill proportional in every power state.
            let x = if badge.is_empty() { 9. } else { 2. };
            self.rounded_path([x, 8., 22., 16.], 3.);
            cairo_stroke(c);
            cairo_move_to(c, (x + 24.) as _, 13.);
            cairo_line_to(c, (x + 24.) as _, 19.);
            cairo_stroke(c);
            if let Some(level) = level.filter(|&level| level > 0) {
                self.rounded([x + 3., 11., 16. * f32::from(level) / 100., 10.], ink, 1.);
            }

            match badge {
                "charging" | "fast-charging" => {
                    let centers: &[f64] = if badge == "fast-charging" {
                        &[33., 40.5]
                    } else {
                        &[36.5]
                    };
                    for &center in centers {
                        cairo_move_to(c, center + 2., 8.);
                        cairo_line_to(c, center - 2.5, 16.);
                        cairo_line_to(c, center + 2., 16.);
                        cairo_line_to(c, center - 2., 24.);
                        cairo_stroke(c);
                    }
                }
                "plugged" => {
                    cairo_move_to(c, 34., 8.);
                    cairo_line_to(c, 34., 12.);
                    cairo_move_to(c, 40., 8.);
                    cairo_line_to(c, 40., 12.);
                    cairo_move_to(c, 32., 12.);
                    cairo_line_to(c, 42., 12.);
                    cairo_move_to(c, 33., 12.);
                    cairo_line_to(c, 33., 16.);
                    cairo_curve_to(c, 33., 21., 41., 21., 41., 16.);
                    cairo_line_to(c, 41., 12.);
                    cairo_move_to(c, 37., 20.);
                    cairo_line_to(c, 37., 24.);
                    cairo_stroke(c);
                }
                _ => {}
            }
            cairo_restore(c);
        }
        true
    }

    /// Shared optical-size vectors for small spatial chrome and status controls.
    /// These remain independent of application artwork and font fallback glyphs.
    pub fn symbolic(&self, rect: [f32; 4], id: &str, ink: [f32; 4]) -> bool {
        let Some(name) = id.strip_prefix("ukui-xr-") else {
            return false;
        };
        if let Some(battery) = name.strip_prefix("battery-") {
            return self.battery_symbol(rect, battery, ink);
        }
        if ![
            "launcher",
            "network",
            "network-offline",
            "wired",
            "audio",
            "audio-muted",
            "status-settings",
            "weather-unavailable",
            "close",
            "minimize",
            "curve",
            "more",
        ]
        .contains(&name)
        {
            return false;
        }
        unsafe {
            let c = self.cr;
            cairo_save(c);
            cairo_translate(c, rect[0] as _, rect[1] as _);
            cairo_scale(c, rect[2] as f64 / 32., rect[3] as f64 / 32.);
            cairo_set_source_rgba(c, ink[0] as _, ink[1] as _, ink[2] as _, ink[3] as _);
            cairo_set_line_width(c, 2.1);
            cairo_set_line_cap(c, 1);
            cairo_new_path(c);
            match name {
                "launcher" | "more" => {
                    let ys: &[f64] = if name == "more" {
                        &[16.]
                    } else {
                        &[8., 16., 24.]
                    };
                    for y in ys {
                        for x in [8., 16., 24.] {
                            cairo_new_path(c);
                            cairo_arc(c, x, *y, 1.9, 0., std::f64::consts::TAU);
                            cairo_fill(c);
                        }
                    }
                }
                "network" | "network-offline" => {
                    for r in [7., 13., 19.] {
                        cairo_new_path(c);
                        cairo_arc(
                            c,
                            16.,
                            27.,
                            r,
                            -3. * std::f64::consts::FRAC_PI_4,
                            -std::f64::consts::FRAC_PI_4,
                        );
                        cairo_stroke(c);
                    }
                    cairo_arc(c, 16., 27., 1.7, 0., std::f64::consts::TAU);
                    cairo_fill(c);
                    if name.ends_with("offline") {
                        cairo_move_to(c, 4., 5.);
                        cairo_line_to(c, 28., 29.);
                        cairo_stroke(c);
                    }
                }
                "wired" => {
                    self.rounded_path([4., 5., 24., 17.], 2.);
                    cairo_stroke(c);
                    cairo_move_to(c, 16., 22.);
                    cairo_line_to(c, 16., 28.);
                    cairo_move_to(c, 10., 28.);
                    cairo_line_to(c, 22., 28.);
                    cairo_stroke(c);
                }
                "audio" | "audio-muted" => {
                    cairo_move_to(c, 4., 12.);
                    cairo_line_to(c, 10., 12.);
                    cairo_line_to(c, 17., 6.);
                    cairo_line_to(c, 17., 26.);
                    cairo_line_to(c, 10., 20.);
                    cairo_line_to(c, 4., 20.);
                    cairo_close_path(c);
                    cairo_fill(c);
                    if name == "audio" {
                        for r in [6., 11.] {
                            cairo_new_path(c);
                            cairo_arc(c, 17., 16., r, -0.8, 0.8);
                            cairo_stroke(c);
                        }
                    } else {
                        cairo_move_to(c, 23., 12.);
                        cairo_line_to(c, 29., 20.);
                        cairo_move_to(c, 29., 12.);
                        cairo_line_to(c, 23., 20.);
                        cairo_stroke(c);
                    }
                }
                "status-settings" => {
                    for i in 0..32 {
                        let a = i as f64 * std::f64::consts::TAU / 32.;
                        let r = if i % 4 == 0 || i % 4 == 3 { 12.5 } else { 10. };
                        let (x, y) = (16. + r * a.cos(), 16. + r * a.sin());
                        if i == 0 {
                            cairo_move_to(c, x, y);
                        } else {
                            cairo_line_to(c, x, y);
                        }
                    }
                    cairo_close_path(c);
                    cairo_stroke(c);
                    cairo_arc(c, 16., 16., 4.4, 0., std::f64::consts::TAU);
                    cairo_stroke(c);
                }
                "weather-unavailable" => {
                    cairo_move_to(c, 8., 24.);
                    cairo_curve_to(c, 0., 24., 1., 14., 8., 14.);
                    cairo_curve_to(c, 8., 3., 25., 3., 25., 15.);
                    cairo_curve_to(c, 33., 15., 32., 24., 25., 24.);
                    cairo_line_to(c, 8., 24.);
                    cairo_stroke(c);
                    cairo_move_to(c, 12., 28.);
                    cairo_line_to(c, 21., 28.);
                    cairo_stroke(c);
                }
                "close" => {
                    cairo_move_to(c, 10., 10.);
                    cairo_line_to(c, 22., 22.);
                    cairo_move_to(c, 22., 10.);
                    cairo_line_to(c, 10., 22.);
                    cairo_stroke(c);
                }
                "minimize" => {
                    cairo_move_to(c, 9., 17.);
                    cairo_line_to(c, 23., 17.);
                    cairo_stroke(c);
                }
                "curve" => {
                    cairo_move_to(c, 8., 8.);
                    cairo_curve_to(c, 13., 11., 19., 11., 24., 8.);
                    cairo_line_to(c, 24., 24.);
                    cairo_curve_to(c, 19., 21., 13., 21., 8., 24.);
                    cairo_close_path(c);
                    cairo_stroke(c);
                }
                _ => unreachable!(),
            }
            cairo_restore(c);
        }
        true
    }
    pub fn icon(&self, rect: [f32; 4], id: &str) -> bool {
        if self.symbolic(rect, id, [35. / 255., 48. / 255., 71. / 255., 1.]) {
            return true;
        }
        if let Some(bitmap) = crate::art::icon(id) {
            self.bitmap(rect, bitmap, None, 1.);
            return true;
        }
        let Some(path) = icon_path(id) else {
            return false;
        };
        let Ok(path_name) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else {
            return false;
        };
        unsafe {
            if path.extension().is_some_and(|s| s == "svg") {
                let h = rsvg_handle_new_from_file(path_name.as_ptr(), std::ptr::null_mut());
                if h.is_null() {
                    return false;
                }
                if id.ends_with("symbolic")
                    || [
                        "window-close",
                        "window-minimize",
                        "go-next",
                        "go-previous",
                        "preferences-system",
                        "view-app-grid",
                    ]
                    .contains(&id)
                {
                    let css = b"svg {color:#233047} path {fill:currentColor}";
                    rsvg_handle_set_stylesheet(h, css.as_ptr(), css.len(), std::ptr::null_mut());
                }
                let viewport = SvgRectangle {
                    x: rect[0] as _,
                    y: rect[1] as _,
                    width: rect[2] as _,
                    height: rect[3] as _,
                };
                let ok =
                    rsvg_handle_render_document(h, self.cr, &viewport, std::ptr::null_mut()) != 0;
                g_object_unref(h);
                ok
            } else {
                let surface = cairo_image_surface_create_from_png(path_name.as_ptr());
                if cairo_surface_status(surface) != 0 {
                    cairo_surface_destroy(surface);
                    return false;
                }
                // Cairo scales the native package icon without altering its palette.
                unsafe extern "C" {
                    fn cairo_image_surface_get_width(s: Ptr) -> i32;
                    fn cairo_image_surface_get_height(s: Ptr) -> i32;
                }
                let w = cairo_image_surface_get_width(surface);
                let h = cairo_image_surface_get_height(surface);
                cairo_save(self.cr);
                cairo_translate(self.cr, rect[0] as _, rect[1] as _);
                cairo_scale(
                    self.cr,
                    rect[2] as f64 / w as f64,
                    rect[3] as f64 / h as f64,
                );
                cairo_set_source_surface(self.cr, surface, 0., 0.);
                cairo_paint(self.cr);
                cairo_restore(self.cr);
                cairo_surface_destroy(surface);
                true
            }
        }
    }
}
