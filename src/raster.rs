// SPDX-License-Identifier: GPL-3.0-or-later
//! CPU scene rasterisation is an explicit M2 baseline. Vulkan composites the
//! resulting planes and application snapshots into both real XR swapchains.
use anyhow::{Result, ensure};
use std::ffi::{c_char, c_double, c_int, c_void};
use xr_shell_protocols::Node;
use xr_spatial_kit::Theme;
type Ptr = *mut c_void;
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
    fn cairo_move_to(cr: Ptr, x: c_double, y: c_double);
    fn cairo_close_path(cr: Ptr);
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
        unsafe {
            let text = text.replace('\0', " ");
            let c = self.cr;
            let layout = pango_cairo_create_layout(c);
            let desc = pango_font_description_from_string(c"Noto Sans CJK SC, sans".as_ptr());
            pango_font_description_set_absolute_size(desc, size as f64 * 1024.);
            pango_layout_set_font_description(layout, desc);
            pango_layout_set_text(layout, text.as_ptr() as _, text.len() as _);
            pango_layout_set_width(layout, (rect[2] * 1024.) as _);
            pango_layout_set_height(layout, (rect[3] * 1024.) as _);
            pango_layout_set_ellipsize(layout, 3);
            cairo_save(c);
            cairo_rectangle(c, rect[0] as _, rect[1] as _, rect[2] as _, rect[3] as _);
            cairo_clip(c);
            cairo_move_to(c, rect[0] as _, rect[1] as _);
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
    _dark: bool,
) {
    let [x, y, w, h] = rect;
    let primary = theme.colour("text.primary");
    let secondary = theme.colour("text.secondary");
    let ink = if node.enabled { primary } else { secondary };
    match node.kind.as_str() {
        "Group" => {}
        "Card" => canvas.rounded(
            rect,
            theme.colour(&node.style_id),
            if node.style_id == "dock.background" {
                32.
            } else {
                16.
            },
        ),
        "Button" => {
            let active = ["button.primary", "dock.active"].contains(&node.style_id.as_str());
            let colour = if pressed {
                [55. / 255., 144. / 255., 250. / 255., 1.]
            } else if hovered {
                [1., 1., 1., 0.24]
            } else {
                theme.colour(&node.style_id)
            };
            let radius = if node.style_id.starts_with("dock.") {
                h / 2.
            } else if node.style_id == "keyboard.key" {
                8.
            } else {
                12.
            };
            canvas.rounded(rect, colour, radius);
            let has_icon = !node.icon_id.is_empty();
            let icon_size = (h - 12.).min(32.).max(16.);
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
                canvas.text(
                    [
                        x + left,
                        y + (h - 30.).max(0.) / 2.,
                        (w - left - 8.).max(1.),
                        h.min(36.),
                    ],
                    &node.text,
                    if active || node.style_id=="keyboard.key" { [1.; 4] } else { ink },
                    if node.style_id == "keyboard.key" {
                        26.
                    } else {
                        22.
                    },
                );
            }
        }
        "Slider" => {
            canvas.rounded([x, y + h / 2. - 3., w, 6.], [1., 1., 1., 0.15], 3.);
            let value = ((node.value - node.min) / (node.max - node.min).max(0.0001)).clamp(0., 1.);
            canvas.rounded(
                [x, y + h / 2. - 3., w * value, 6.],
                [55. / 255., 144. / 255., 250. / 255., 1.],
                3.,
            );
            canvas.rounded(
                [x + (w - 20.) * value, y + h / 2. - 10., 20., 20.],
                [1.; 4],
                10.,
            );
        }
        "Text" => canvas.text(
            rect,
            &node.text,
            if node.style_id == "text.secondary" {
                secondary
            } else {
                primary
            },
            if node.node_id.contains("title") {
                28.
            } else {
                22.
            },
        ),
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
    pub fn icon(&self, rect: [f32; 4], id: &str) -> bool {
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
                    let css = b"svg {color:#F2F6FC} path {fill:currentColor}";
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
