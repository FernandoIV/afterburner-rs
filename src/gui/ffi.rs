//! Small GTK4/GObject/Cairo ABI surface. Widgets are only used on the GTK thread.
#![allow(dead_code)]
use std::ffi::{c_char, c_int, c_uint, c_void};
pub type Ptr = *mut c_void;
pub type Callback = unsafe extern "C" fn();

unsafe extern "C" {
    pub fn gtk_application_new(id: *const c_char, flags: c_int) -> Ptr;
    pub fn g_application_run(app: Ptr, argc: c_int, argv: *mut *mut c_char) -> c_int;
    pub fn g_object_unref(object: Ptr);
    pub fn g_signal_connect_data(object: Ptr, signal: *const c_char, callback: Option<Callback>, data: Ptr,
        destroy: Option<unsafe extern "C" fn(Ptr, Ptr)>, flags: c_uint) -> u64;
    pub fn g_signal_emit_by_name(object: Ptr, signal: *const c_char, ...);
    pub fn g_timeout_add(ms: c_uint, callback: Option<unsafe extern "C" fn(Ptr) -> c_int>, data: Ptr) -> c_uint;
    pub fn g_source_remove(id: c_uint) -> c_int;
    pub fn g_type_name_from_instance(object: Ptr) -> *const c_char;

    pub fn gtk_application_window_new(app: Ptr) -> Ptr;
    pub fn gtk_window_set_title(window: Ptr, title: *const c_char);
    pub fn gtk_window_set_default_size(window: Ptr, width: c_int, height: c_int);
    pub fn gtk_window_set_child(window: Ptr, child: Ptr);
    pub fn gtk_window_set_titlebar(window: Ptr, titlebar: Ptr);
    pub fn gtk_window_set_icon_name(window: Ptr, name: *const c_char);
    pub fn gtk_window_present(window: Ptr);
    pub fn gtk_window_destroy(window: Ptr);
    pub fn gtk_window_close(window: Ptr);
    pub fn gtk_header_bar_new() -> Ptr;
    pub fn gtk_header_bar_set_title_widget(bar: Ptr, child: Ptr);
    pub fn gtk_header_bar_pack_start(bar: Ptr, child: Ptr);
    pub fn gtk_header_bar_pack_end(bar: Ptr, child: Ptr);
    pub fn gtk_image_new_from_icon_name(name: *const c_char) -> Ptr;
    pub fn gtk_image_set_pixel_size(image: Ptr, size: c_int);

    pub fn gtk_widget_add_css_class(widget: Ptr, class: *const c_char);
    pub fn gtk_widget_set_sensitive(widget: Ptr, enabled: c_int);
    pub fn gtk_widget_set_hexpand(widget: Ptr, expand: c_int);
    pub fn gtk_widget_set_vexpand(widget: Ptr, expand: c_int);
    pub fn gtk_widget_set_halign(widget: Ptr, align: c_int);
    pub fn gtk_widget_set_valign(widget: Ptr, align: c_int);
    pub fn gtk_widget_set_margin_start(widget: Ptr, margin: c_int);
    pub fn gtk_widget_set_margin_end(widget: Ptr, margin: c_int);
    pub fn gtk_widget_set_margin_top(widget: Ptr, margin: c_int);
    pub fn gtk_widget_set_margin_bottom(widget: Ptr, margin: c_int);
    pub fn gtk_widget_set_size_request(widget: Ptr, width: c_int, height: c_int);
    pub fn gtk_widget_set_tooltip_text(widget: Ptr, text: *const c_char);
    pub fn gtk_widget_queue_draw(widget: Ptr);
    pub fn gtk_widget_add_controller(widget: Ptr, controller: Ptr);
    pub fn gtk_widget_get_width(widget: Ptr) -> c_int;
    pub fn gtk_widget_get_height(widget: Ptr) -> c_int;
    pub fn gtk_widget_get_first_child(widget: Ptr) -> Ptr;
    pub fn gtk_widget_get_next_sibling(widget: Ptr) -> Ptr;
    pub fn gtk_widget_get_native(widget: Ptr) -> Ptr;

    pub fn gtk_box_new(orientation: c_int, spacing: c_int) -> Ptr;
    pub fn gtk_box_append(parent: Ptr, child: Ptr);
    pub fn gtk_box_remove(parent: Ptr, child: Ptr);
    pub fn gtk_grid_new() -> Ptr;
    pub fn gtk_grid_attach(grid: Ptr, child: Ptr, column: c_int, row: c_int, width: c_int, height: c_int);
    pub fn gtk_grid_set_row_spacing(grid: Ptr, spacing: c_uint);
    pub fn gtk_grid_set_column_spacing(grid: Ptr, spacing: c_uint);
    pub fn gtk_grid_set_column_homogeneous(grid: Ptr, homogeneous: c_int);
    pub fn gtk_scrolled_window_new() -> Ptr;
    pub fn gtk_scrolled_window_set_child(scroller: Ptr, child: Ptr);
    pub fn gtk_scrolled_window_set_policy(scroller: Ptr, horizontal: c_int, vertical: c_int);
    pub fn gtk_stack_new() -> Ptr;
    pub fn gtk_stack_add_titled(stack: Ptr, child: Ptr, name: *const c_char, title: *const c_char) -> Ptr;
    pub fn gtk_stack_set_visible_child_name(stack: Ptr, name: *const c_char);
    pub fn gtk_stack_set_transition_type(stack: Ptr, transition: c_int);
    pub fn gtk_stack_set_transition_duration(stack: Ptr, duration: c_uint);
    pub fn gtk_stack_sidebar_new() -> Ptr;
    pub fn gtk_stack_sidebar_set_stack(sidebar: Ptr, stack: Ptr);

    pub fn gtk_label_new(text: *const c_char) -> Ptr;
    pub fn gtk_label_set_text(label: Ptr, text: *const c_char);
    pub fn gtk_label_set_xalign(label: Ptr, align: f32);
    pub fn gtk_label_set_wrap(label: Ptr, wrap: c_int);
    pub fn gtk_label_set_selectable(label: Ptr, selectable: c_int);
    pub fn gtk_label_set_ellipsize(label: Ptr, mode: c_int);
    pub fn gtk_button_new_with_label(text: *const c_char) -> Ptr;
    pub fn gtk_button_set_child(button: Ptr, child: Ptr);
    pub fn gtk_entry_new() -> Ptr;
    pub fn gtk_editable_get_text(editable: Ptr) -> *const c_char;
    pub fn gtk_editable_set_text(editable: Ptr, text: *const c_char);
    pub fn gtk_entry_set_placeholder_text(entry: Ptr, text: *const c_char);
    pub fn gtk_switch_new() -> Ptr;
    pub fn gtk_switch_get_active(switch: Ptr) -> c_int;
    pub fn gtk_switch_set_active(switch: Ptr, active: c_int);
    pub fn gtk_check_button_new_with_label(label: *const c_char) -> Ptr;
    pub fn gtk_check_button_get_active(check: Ptr) -> c_int;
    pub fn gtk_check_button_set_active(check: Ptr, active: c_int);
    pub fn gtk_scale_new_with_range(orientation: c_int, min: f64, max: f64, step: f64) -> Ptr;
    pub fn gtk_scale_set_draw_value(scale: Ptr, draw: c_int);
    pub fn gtk_scale_set_digits(scale: Ptr, digits: c_int);
    pub fn gtk_range_get_value(range: Ptr) -> f64;
    pub fn gtk_range_set_value(range: Ptr, value: f64);
    pub fn gtk_range_set_range(range: Ptr, min: f64, max: f64);
    pub fn gtk_spin_button_new_with_range(min: f64, max: f64, step: f64) -> Ptr;
    pub fn gtk_spin_button_get_value(spin: Ptr) -> f64;
    pub fn gtk_spin_button_set_value(spin: Ptr, value: f64);
    pub fn gtk_spin_button_update(spin: Ptr);
    pub fn gtk_combo_box_text_new() -> Ptr;
    pub fn gtk_combo_box_text_append_text(combo: Ptr, text: *const c_char);
    pub fn gtk_combo_box_text_remove_all(combo: Ptr);
    pub fn gtk_combo_box_set_active(combo: Ptr, active: c_int);
    pub fn gtk_combo_box_get_active(combo: Ptr) -> c_int;
    pub fn gtk_progress_bar_new() -> Ptr;
    pub fn gtk_progress_bar_set_fraction(bar: Ptr, fraction: f64);

    pub fn gtk_drawing_area_new() -> Ptr;
    pub fn gtk_drawing_area_set_draw_func(area: Ptr,
        draw: Option<unsafe extern "C" fn(Ptr, Ptr, c_int, c_int, Ptr)>, data: Ptr,
        destroy: Option<unsafe extern "C" fn(Ptr)>);
    pub fn gtk_gesture_click_new() -> Ptr;
    pub fn gtk_event_controller_motion_new() -> Ptr;
    pub fn gtk_message_dialog_new(parent: Ptr, flags: c_int, kind: c_int, buttons: c_int, format: *const c_char, ...) -> Ptr;
    pub fn gtk_dialog_add_button(dialog: Ptr, text: *const c_char, response: c_int) -> Ptr;

    pub fn gtk_css_provider_new() -> Ptr;
    pub fn gtk_css_provider_load_from_data(provider: Ptr, data: *const c_char, length: isize);
    pub fn gdk_display_get_default() -> Ptr;
    pub fn gtk_style_context_add_provider_for_display(display: Ptr, provider: Ptr, priority: c_uint);

    // Widget-only snapshots for repeatable UI tests; not desktop screenshots.
    pub fn gtk_widget_paintable_new(widget: Ptr) -> Ptr;
    pub fn gdk_paintable_snapshot(paintable: Ptr, snapshot: Ptr, width: f64, height: f64);
    pub fn gtk_snapshot_new() -> Ptr;
    pub fn gtk_snapshot_free_to_node(snapshot: Ptr) -> Ptr;
    pub fn gtk_native_get_renderer(native: Ptr) -> Ptr;
    pub fn gsk_renderer_render_texture(renderer: Ptr, node: Ptr, viewport: Ptr) -> Ptr;
    pub fn gdk_texture_save_to_png(texture: Ptr, filename: *const c_char) -> c_int;
    pub fn gsk_render_node_unref(node: Ptr);

    pub fn cairo_set_source_rgb(cr: Ptr, r: f64, g: f64, b: f64);
    pub fn cairo_set_source_rgba(cr: Ptr, r: f64, g: f64, b: f64, a: f64);
    pub fn cairo_set_line_width(cr: Ptr, width: f64);
    pub fn cairo_move_to(cr: Ptr, x: f64, y: f64);
    pub fn cairo_line_to(cr: Ptr, x: f64, y: f64);
    pub fn cairo_stroke(cr: Ptr);
    pub fn cairo_fill(cr: Ptr);
    pub fn cairo_close_path(cr: Ptr);
    pub fn cairo_rectangle(cr: Ptr, x: f64, y: f64, width: f64, height: f64);
    pub fn cairo_arc(cr: Ptr, x: f64, y: f64, radius: f64, start: f64, end: f64);
    pub fn cairo_select_font_face(cr: Ptr, family: *const c_char, slant: c_int, weight: c_int);
    pub fn cairo_set_font_size(cr: Ptr, size: f64);
    pub fn cairo_show_text(cr: Ptr, text: *const c_char);
}
