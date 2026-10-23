//! Native GTK4 desktop application. No browser, web view, terminal widget or HTTP client.
//! The only custom drawing is the two Cairo charts; all controls are GTK widgets.
mod ffi;
mod worker;
use ffi::*;
use worker::{Event, Reading, Work};
use crate::{config, drivers::ApplySettings};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::{CStr, CString};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

const PAGES: [(&str, &str); 6] = [
    ("monitor", "Overview"), ("controls", "Power & clocks"),
    ("fans", "Fans"), ("profiles", "Profiles"),
    ("sensors", "Sensors & IPC"), ("settings", "Logging & service"),
];

#[derive(Clone)]
enum Action {
    Page(&'static str), Power, Offsets, Locks, ManualFan, AutoFan,
    ConfirmReset, Reset, Preset, AddNode, UpdateNode, RemoveNode, CurveOn, CurveOff,
    SaveProfile(u32), ConfirmSave(u32), LoadProfile(u32), SaveSettings,
    OpenConfig, OpenLog, Service(&'static str),
}
struct Metric { value: Ptr, detail: Ptr, bar: Ptr }
struct Smoke { dir: PathBuf, tick: u32, page: usize, captures: usize }
struct Ui {
    window: Ptr, stack: Ptr, gpu_combo: Ptr, selected: usize, badge: Ptr, status: Ptr,
    gpu_title: Ptr, metrics: Vec<Metric>, history: Vec<VecDeque<(f32, f32)>>,
    chart: Ptr, curve_area: Ptr, curve_size: (f64, f64), curve: Vec<(f32, f32)>,
    drag: Option<usize>, node_combo: Ptr, node_temp: Ptr, node_duty: Ptr,
    preset: Ptr, curve_status: Ptr, fan_status: Ptr, fan: Ptr, power: Ptr, power_hint: Ptr,
    core: Ptr, memory: Ptr, lock_core: [Ptr; 2], lock_mem: [Ptr; 2],
    lock_core_on: Ptr, lock_mem_on: Ptr, admin: Ptr,
    log_on: Ptr, log_path: Ptr, poll_period: Ptr, api_on: Ptr,
    sensor_table: Ptr, ipc_status: Ptr, profile_name: Ptr, profile_labels: Vec<Ptr>,
    power_button: Ptr, offsets_button: Ptr, fan_button: Ptr, auto_button: Ptr,
    readings: Vec<Reading>, initialized: bool, tx: Sender<Work>, rx: Receiver<Event>,
    source: u32, ticks: u32, smoke: Option<Smoke>, preset_button: Ptr,
    manual_mode: bool,
    worker: Option<std::thread::JoinHandle<()>>,
}
thread_local! { static UI: RefCell<Option<Ui>> = const { RefCell::new(None) }; }

fn c(s: &str) -> CString { CString::new(s.replace('\0', "")).unwrap() }
fn text(w: Ptr, s: &str) { unsafe { gtk_label_set_text(w, c(s).as_ptr()); } }
fn class(w: Ptr, name: &str) { unsafe { gtk_widget_add_css_class(w, c(name).as_ptr()); } }
fn expand(w: Ptr) { unsafe { gtk_widget_set_hexpand(w, 1); } }
fn label(s: &str, cls: &str) -> Ptr {
    unsafe {
        let w = gtk_label_new(c(s).as_ptr()); gtk_label_set_xalign(w, 0.0);
        if !cls.is_empty() { class(w, cls); } w
    }
}
fn wrapped(s: &str, cls: &str) -> Ptr {
    let w = label(s, cls); unsafe { gtk_label_set_wrap(w, 1); } w
}
fn box_(vertical: bool, spacing: i32) -> Ptr { unsafe { gtk_box_new(vertical as i32, spacing) } }
fn append(parent: Ptr, child: Ptr) { unsafe { gtk_box_append(parent, child); } }
fn margin(w: Ptr, n: i32) { unsafe {
    gtk_widget_set_margin_start(w, n); gtk_widget_set_margin_end(w, n);
    gtk_widget_set_margin_top(w, n); gtk_widget_set_margin_bottom(w, n);
} }
fn grid() -> Ptr { unsafe {
    let g = gtk_grid_new(); gtk_grid_set_column_spacing(g, 14); gtk_grid_set_row_spacing(g, 14); g
} }
fn attach(g: Ptr, w: Ptr, x: i32, y: i32, width: i32) { unsafe { gtk_grid_attach(g, w, x, y, width, 1); } }
fn panel(title: &str, subtitle: &str) -> Ptr {
    let b = box_(true, 12); class(b, "card"); expand(b);
    append(b, label(title, "card-title"));
    if !subtitle.is_empty() { append(b, wrapped(subtitle, "subtle")); } b
}
fn spin(min: f64, max: f64, initial: f64, step: f64) -> Ptr { unsafe {
    let w = gtk_spin_button_new_with_range(min, max, step); gtk_spin_button_set_value(w, initial); w
} }
fn spin_value(w: Ptr) -> f64 { unsafe { gtk_spin_button_update(w); gtk_spin_button_get_value(w) } }
fn slider(min: f64, max: f64, initial: f64) -> Ptr { unsafe {
    let w = gtk_scale_new_with_range(0, min, max, 1.0);
    gtk_scale_set_draw_value(w, 1); gtk_scale_set_digits(w, 0); gtk_range_set_value(w, initial); expand(w); w
} }
fn entry(initial: &str) -> Ptr { unsafe {
    let w = gtk_entry_new(); gtk_editable_set_text(w, c(initial).as_ptr()); expand(w); w
} }
fn entry_value(w: Ptr) -> String { unsafe { CStr::from_ptr(gtk_editable_get_text(w)).to_string_lossy().into_owned() } }
fn combo(items: &[&str]) -> Ptr { unsafe {
    let w = gtk_combo_box_text_new(); for s in items { gtk_combo_box_text_append_text(w, c(s).as_ptr()); }
    gtk_combo_box_set_active(w, 0); w
} }
fn clear_box(w: Ptr) { unsafe {
    loop { let child = gtk_widget_get_first_child(w); if child.is_null() { break; } gtk_box_remove(w, child); }
} }

unsafe fn callback(p: *const ()) -> Callback { std::mem::transmute(p) }
unsafe extern "C" fn free_action(data: Ptr, _: Ptr) { drop(Box::from_raw(data.cast::<Action>())); }
fn button(title: &str, icon: &str, cls: &str, action: Action) -> Ptr { unsafe {
    let w = gtk_button_new_with_label(c(title).as_ptr());
    if !icon.is_empty() {
        let row = box_(false, 7); let image = gtk_image_new_from_icon_name(c(icon).as_ptr());
        append(row, image); append(row, label(title, "")); gtk_button_set_child(w, row);
    }
    if !cls.is_empty() { class(w, cls); }
    gtk_widget_set_tooltip_text(w, c(title).as_ptr());
    gtk_widget_set_halign(w, 1);
    gtk_widget_set_valign(w, 3);
    gtk_widget_set_sensitive(w, 1);
    g_signal_connect_data(w, c"clicked".as_ptr(), Some(callback(clicked as *const ())),
        Box::into_raw(Box::new(action)).cast(), Some(free_action), 0);
    w
} }
fn guarded(f: impl FnOnce()) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err() {
        eprintln!("A GTK interface error was contained");
    }
}
fn with_ui(f: impl FnOnce(&mut Ui)) {
    UI.with(|slot| { if let Ok(mut value) = slot.try_borrow_mut() { if let Some(ui) = value.as_mut() { f(ui); } } });
}
unsafe extern "C" fn clicked(_: Ptr, data: Ptr) {
    guarded(|| { let a = (&*data.cast::<Action>()).clone(); with_ui(|ui| ui.act(a)); });
}
unsafe extern "C" fn gpu_changed(_: Ptr, _: Ptr) {
    guarded(|| with_ui(|ui| {
        let i = gtk_combo_box_get_active(ui.gpu_combo);
        if i >= 0 && (i as usize) < ui.readings.len() { ui.selected = i as usize; ui.sync_controls(); ui.render_readings(); }
    }));
}
unsafe extern "C" fn node_changed(_: Ptr, _: Ptr) {
    guarded(|| with_ui(|ui| ui.sync_node()));
}
unsafe extern "C" fn close(_: Ptr, _: Ptr) -> i32 {
    guarded(|| with_ui(|ui| {
        if ui.source != 0 { g_source_remove(ui.source); ui.source = 0; }
        let _ = ui.tx.send(Work::Stop);
    }));
    0 // let GTK destroy the window and release the application hold
}

pub fn run(args: &[String]) -> i32 {
    let smoke = args.iter().position(|a| a == "--smoke-test")
        .and_then(|i| args.get(i + 1)).map(PathBuf::from);
    unsafe {
        let app = gtk_application_new(c"io.github.afterburnerrs.Native".as_ptr(), if smoke.is_some() { 32 } else { 0 });
        let data = Box::into_raw(Box::new(smoke));
        g_signal_connect_data(app, c"activate".as_ptr(), Some(callback(activate as *const ())), data.cast(), None, 0);
        // No borrowed CString/argv survives its owner. GtkApplication gets no CLI arguments.
        let code = g_application_run(app, 0, std::ptr::null_mut());
        drop(Box::from_raw(data));
        g_object_unref(app);
        // Let the worker return manual fans to automatic before main exits the process.
        let finished = UI.with(|slot| slot.borrow_mut().take());
        if let Some(mut ui) = finished {
            let _ = ui.tx.send(Work::Stop);
            if let Some(handle) = ui.worker.take() { let _ = handle.join(); }
        }
        code
    }
}
unsafe extern "C" fn activate(app: Ptr, data: Ptr) {
    guarded(|| {
        let existing = UI.with(|slot| slot.borrow().as_ref().map(|ui| ui.window));
        if let Some(window) = existing { gtk_window_present(window); return; }
        let smoke = (&*data.cast::<Option<PathBuf>>()).clone();
        let mut ui = build(app, smoke);
        ui.source = g_timeout_add(100, Some(tick), std::ptr::null_mut());
        let window = ui.window;
        UI.with(|slot| *slot.borrow_mut() = Some(ui));
        gtk_window_present(window);
    });
}

unsafe fn build(app: Ptr, smoke: Option<PathBuf>) -> Ui {
    let css = gtk_css_provider_new();
    let style = include_str!("assets/native.css");
    gtk_css_provider_load_from_data(css, style.as_ptr().cast(), style.len() as isize);
    gtk_style_context_add_provider_for_display(gdk_display_get_default(), css, 600);
    g_object_unref(css);
    let window = gtk_application_window_new(app);
    gtk_window_set_title(window, c"Afterburner RS".as_ptr());
    gtk_window_set_icon_name(window, c"afterburner-rs".as_ptr());
    gtk_window_set_default_size(window, 1160, 850);
    g_signal_connect_data(window, c"close-request".as_ptr(), Some(callback(close as *const ())), std::ptr::null_mut(), None, 0);

    let header = gtk_header_bar_new();
    let brand = label("AFTERBURNER  //  RS", "brand");
    gtk_header_bar_set_title_widget(header, brand);
    let image = gtk_image_new_from_icon_name(c"afterburner-rs".as_ptr()); gtk_image_set_pixel_size(image, 30);
    gtk_header_bar_pack_start(header, image);
    let badge = label("CONNECTING", "badge"); gtk_header_bar_pack_end(header, badge);
    gtk_window_set_titlebar(window, header);

    let shell = box_(false, 0); gtk_window_set_child(window, shell);
    let stack = gtk_stack_new(); gtk_stack_set_transition_type(stack, 1); gtk_stack_set_transition_duration(stack, 180);
    if smoke.is_some() { gtk_stack_set_transition_type(stack, 0); }
    expand(stack); gtk_widget_set_vexpand(stack, 1);
    let sidebar = gtk_stack_sidebar_new(); gtk_stack_sidebar_set_stack(sidebar, stack);
    class(sidebar, "sidebar"); gtk_widget_set_size_request(sidebar, 175, -1); append(shell, sidebar);
    let body = box_(true, 12); margin(body, 16); expand(body); append(shell, body);
    let top = box_(false, 12); append(body, top);
    let gpu_title = label("Detecting graphics cards…", "card-title"); expand(gpu_title); append(top, gpu_title);
    let gpu_combo = combo(&["Detecting GPUs…"]); append(top, gpu_combo);
    append(body, stack);
    let status = wrapped("Ready. Settings are not applied until you click their Apply button.", "status"); append(body, status);

    let mut pages = Vec::new();
    for (id, title) in PAGES {
        let scroll = gtk_scrolled_window_new(); gtk_scrolled_window_set_policy(scroll, 2, 1);
        let page = box_(true, 16); margin(page, 3); class(page, "page");
        gtk_scrolled_window_set_child(scroll, page);
        gtk_stack_add_titled(stack, scroll, c(id).as_ptr(), c(title).as_ptr());
        pages.push(page);
    }

    // MONITOR: native metric cards, progress bars and buttons.
    let p = pages[0]; append(p, label("Your GPU at a glance", "page-title"));
    append(p, label("Live hardware readings · refreshed every second", "page-subtitle"));
    let quick = box_(false, 10);
    append(quick, button("Tune GPU", "preferences-system-symbolic", "primary", Action::Page("controls")));
    append(quick, button("Fan control", "weather-windy-symbolic", "accent", Action::Page("fans")));
    append(quick, button("Restore defaults", "edit-undo-symbolic", "", Action::ConfirmReset)); append(p, quick);
    let mg = grid(); gtk_grid_set_column_homogeneous(mg, 1); append(p, mg);
    let mut metrics = Vec::new();
    for (i, (name, unit)) in [("TEMPERATURE", "°C"), ("GPU USAGE", "%"), ("POWER DRAW", "W"),
        ("FAN SPEED", "%"), ("CORE CLOCK", "MHz"), ("MEMORY CLOCK", "MHz")].iter().enumerate() {
        let card = box_(true, 5); class(card, "metric"); if i == 2 || i == 3 { class(card, "accent"); }
        expand(card); append(card, label(name, "caption"));
        let row = box_(false, 7); let value = label("—", "metric-value"); append(row, value);
        let u = label(unit, "unit"); gtk_widget_set_valign(u, 2); append(row, u); append(card, row);
        let detail = label("Waiting for a reading", "subtle"); append(card, detail);
        let bar = gtk_progress_bar_new(); append(card, bar);
        attach(mg, card, i as i32 % 3, i as i32 / 3, 1);
        metrics.push(Metric { value, detail, bar });
    }
    let chart_card = panel("Live history", "Cyan: temperature (°C)     ·     Orange: GPU usage (%)     ·     Last 180 readings");
    let chart = gtk_drawing_area_new(); gtk_widget_set_size_request(chart, -1, 165); expand(chart);
    gtk_drawing_area_set_draw_func(chart, Some(draw), std::ptr::null_mut(), None); append(chart_card, chart); append(p, chart_card);

    // POWER & CLOCKS: two-column native settings cards, never a rendered console.
    let p = pages[1]; append(p, label("Power & clocks", "page-title"));
    append(p, wrapped("Changes are manual. Higher clocks can cause instability; watch temperatures and respect your hardware limits.", "page-subtitle"));
    let cg = grid(); gtk_grid_set_column_homogeneous(cg, 1); append(p, cg);
    let power_card = panel("Power limit", "Uses the adjustment range reported by your driver.");
    let power = slider(50.0, 400.0, 100.0); append(power_card, power);
    let power_hint = label("Reading power limits…", "subtle"); append(power_card, power_hint);
    let power_button = button("Apply power limit", "power-profile-performance-symbolic", "primary", Action::Power);
    append(power_card, power_button); attach(cg, power_card, 0, 0, 1);
    let clocks = panel("Clock offsets", "Offset from the driver's clock, in MHz. Support depends on the GPU and driver.");
    let core = spin(-2000.0, 2000.0, 0.0, 5.0); let memory = spin(-5000.0, 5000.0, 0.0, 25.0);
    field(clocks, "Core", core); field(clocks, "Memory", memory);
    let offsets_button = button("Apply offsets", "speedometer-symbolic", "", Action::Offsets); append(clocks, offsets_button); attach(cg, clocks, 1, 0, 1);
    let locks = panel("Locked clock ranges", "Enable only the ranges you want to lock (MHz). NVIDIA backend only.");
    let lock_core_on = gtk_check_button_new_with_label(c"Lock core clocks".as_ptr()); append(locks, lock_core_on);
    let lock_core = [spin(0.0, 4000.0, 0.0, 15.0), spin(0.0, 4000.0, 0.0, 15.0)]; range_fields(locks, lock_core);
    let lock_mem_on = gtk_check_button_new_with_label(c"Lock memory clocks".as_ptr()); append(locks, lock_mem_on);
    let lock_mem = [spin(0.0, 16000.0, 0.0, 50.0), spin(0.0, 16000.0, 0.0, 50.0)]; range_fields(locks, lock_mem);
    append(locks, button("Apply clock ranges", "object-select-symbolic", "", Action::Locks)); attach(cg, locks, 0, 1, 1);
    let safety = panel("Permissions & recovery", "Protected changes may require system authorization. The window never runs as root.");
    let admin = gtk_check_button_new_with_label(c"Request authorization for power and clocks".as_ptr()); append(safety, admin);
    append(safety, wrapped("Voltage and V/F curve editing are not implemented by this Linux backend. No simulated controls are provided.", "subtle"));
    append(safety, button("Restore driver defaults", "edit-undo-symbolic", "danger", Action::ConfirmReset)); attach(cg, safety, 1, 1, 1);

    // FANS: manual control plus real native graphical/node controls.
    let p = pages[2]; append(p, label("Fan control", "page-title"));
    append(p, label("Automatic control, a manual speed, or a temperature curve", "page-subtitle"));
    let manual = panel("Fan speed", "Supported NVIDIA GPUs use NVML, including on Wayland. Other backends depend on available driver controls.");
    let fan_status = label("Reading fan status…", "subtle"); append(manual, fan_status);
    let fan = slider(30.0, 100.0, 50.0); class(fan, "accent"); append(manual, fan);
    let actions = box_(false, 10);
    let fan_button = button("Apply fan speed", "weather-windy-symbolic", "accent", Action::ManualFan);
    let auto_button = button("Automatic", "view-refresh-symbolic", "", Action::AutoFan);
    append(actions, fan_button); append(actions, auto_button); append(manual, actions); append(p, manual);
    let curve_card = panel("Temperature curve", "Drag points to move them. Double-click a point to remove it, or use the numeric controls.");
    let cr = box_(false, 14); append(curve_card, cr);
    let curve_area = gtk_drawing_area_new(); gtk_widget_set_size_request(curve_area, 420, 235); expand(curve_area);
    gtk_drawing_area_set_draw_func(curve_area, Some(draw), 1usize as Ptr, None); append(cr, curve_area);
    let click = gtk_gesture_click_new();
    g_signal_connect_data(click, c"pressed".as_ptr(), Some(callback(curve_press as *const ())), std::ptr::null_mut(), None, 0);
    g_signal_connect_data(click, c"released".as_ptr(), Some(callback(curve_release as *const ())), std::ptr::null_mut(), None, 0);
    gtk_widget_add_controller(curve_area, click);
    let motion = gtk_event_controller_motion_new();
    g_signal_connect_data(motion, c"motion".as_ptr(), Some(callback(curve_motion as *const ())), std::ptr::null_mut(), None, 0);
    gtk_widget_add_controller(curve_area, motion);
    let editor = box_(true, 9); append(cr, editor);
    let preset = combo(&["Balanced", "Quiet", "Aggressive"]); append(editor, preset);
    let preset_button = button("Load preset", "document-open-symbolic", "", Action::Preset); append(editor, preset_button);
    let node_combo = combo(&["Point 1"]); append(editor, node_combo);
    let node_temp = spin(30.0, 110.0, 40.0, 1.0); let node_duty = spin(0.0, 100.0, 30.0, 1.0);
    field(editor, "Temperature °C", node_temp); field(editor, "Speed %", node_duty);
    let row = box_(false, 6); append(row, button("Update point", "", "", Action::UpdateNode));
    append(row, button("+", "", "", Action::AddNode)); append(row, button("−", "", "", Action::RemoveNode)); append(editor, row);
    let row = box_(false, 10); append(row, button("Enable curve", "object-select-symbolic", "primary", Action::CurveOn));
    append(row, button("Disable curve", "media-playback-stop-symbolic", "", Action::CurveOff)); append(curve_card, row);
    let curve_status = wrapped("Curve disabled", "subtle"); append(curve_card, curve_status); append(p, curve_card);

    // PROFILES: nine independent native cards, no textual CLI output.
    let p = pages[3]; append(p, label("Your profiles", "page-title"));
    append(p, label("Save the control values for the currently selected GPU", "page-subtitle"));
    let profile_name = entry(""); gtk_entry_set_placeholder_text(profile_name, c"Name for the next saved profile…".as_ptr()); append(p, profile_name);
    let pg = grid(); gtk_grid_set_column_homogeneous(pg, 1); append(p, pg);
    let mut profile_labels = Vec::new();
    for s in 1..=9 {
        let card = panel(&format!("Profile {s:02}"), "");
        let detail = wrapped("Empty", "subtle"); gtk_widget_set_size_request(detail, 190, 50); append(card, detail);
        let row = box_(false, 8); append(row, button("Load", "", "", Action::LoadProfile(s)));
        append(row, button("Save", "", "accent", Action::ConfirmSave(s))); append(card, row);
        attach(pg, card, (s as i32 - 1) % 3, (s as i32 - 1) / 3, 1); profile_labels.push(detail);
    }

    // SENSORS: proper native table of cells, plus MACM status.
    let p = pages[4]; append(p, label("Sensors & interoperability", "page-title"));
    append(p, label("Live MAHM data · native shared-memory reader", "page-subtitle"));
    let sensor_table = box_(true, 0); class(sensor_table, "card"); append(p, sensor_table);
    let ipc_status = wrapped("MACM: no client connected", "status"); append(p, ipc_status);

    // SETTINGS: explicit save; switch callbacks never write old state.
    let ini = settings();
    let p = pages[5]; append(p, label("Logging & service", "page-title"));
    append(p, label("This interface does not need a web server or browser", "page-subtitle"));
    let logs = panel("CSV sensor logging", "The service records temperature, fan speed, clocks, usage, and power for each GPU.");
    let log_on = gtk_switch_new(); gtk_switch_set_active(log_on, (ini.get_i64("Settings", "EnableLog").unwrap_or(0) == 1) as i32);
    field(logs, "Enable sensor logging", log_on);
    let log_default = config::config_dir().join("hardware-log.csv");
    let log_path = entry(ini.get("Settings", "LogPath").filter(|p| !p.is_empty()).unwrap_or(log_default.to_str().unwrap_or("")));
    field(logs, "CSV file", log_path);
    let row = box_(false, 8); append(row, button("Open CSV", "document-open-symbolic", "", Action::OpenLog));
    append(row, button("Settings folder", "folder-open-symbolic", "", Action::OpenConfig)); append(logs, row); append(p, logs);
    let svc = panel("Background service", "Publishes MAHM and runs enabled fan curves, even when the window is closed.");
    let poll_period = spin(100.0, 10000.0, ini.get_i64("Settings", "HwPollPeriod").unwrap_or(1000) as f64, 100.0);
    field(svc, "Service polling interval (ms)", poll_period);
    let api_on = gtk_check_button_new_with_label(c"Optional legacy web server (not required by this GUI)".as_ptr());
    gtk_check_button_set_active(api_on, (ini.get_i64("Settings", "EnableServer").unwrap_or(0) == 1) as i32); append(svc, api_on);
    let row = box_(false, 8);
    append(row, button("Start", "media-playback-start-symbolic", "", Action::Service("start")));
    append(row, button("Restart", "view-refresh-symbolic", "", Action::Service("restart")));
    append(row, button("Stop", "media-playback-stop-symbolic", "", Action::Service("stop"))); append(svc, row); append(p, svc);
    append(p, button("Save settings", "document-save-symbolic", "primary", Action::SaveSettings));

    g_signal_connect_data(gpu_combo, c"changed".as_ptr(), Some(callback(gpu_changed as *const ())), std::ptr::null_mut(), None, 0);
    g_signal_connect_data(node_combo, c"changed".as_ptr(), Some(callback(node_changed as *const ())), std::ptr::null_mut(), None, 0);
    let (tx, rx, worker) = worker::start();
    let mut curve = parse_curve(ini.get("Settings", "SoftwareFanCurve").unwrap_or("")).unwrap_or_else(default_curve);
    curve.sort_by(|a,b| a.0.total_cmp(&b.0));
    let mut ui = Ui { window, stack, gpu_combo, selected: 0, badge, status, gpu_title,
        metrics, history: Vec::new(), chart, curve_area, curve_size: (520.0, 235.0), curve, drag: None,
        node_combo, node_temp, node_duty, preset, curve_status, fan_status, fan, power, power_hint,
        core, memory, lock_core, lock_mem, lock_core_on, lock_mem_on, admin, log_on, log_path,
        poll_period, api_on, sensor_table, ipc_status, profile_name, profile_labels, power_button,
        offsets_button, fan_button, auto_button, readings: Vec::new(), initialized: false,
        tx, rx, source: 0, ticks: 0, smoke: smoke.map(|dir| Smoke { dir, tick: 0, page: 0, captures: 0 }), preset_button, manual_mode: false, worker: Some(worker) };
    ui.rebuild_nodes(0); ui.refresh_profiles(); ui.refresh_curve_status();
    ui
}

fn field(parent: Ptr, title: &str, control: Ptr) {
    let row = box_(false, 10); let l = label(title, "subtle"); expand(l);
    append(row, l); append(row, control); append(parent, row);
}
fn range_fields(parent: Ptr, controls: [Ptr; 2]) {
    let row = box_(false, 7); append(row, label("Min.", "subtle")); append(row, controls[0]);
    append(row, label("Max.", "subtle")); append(row, controls[1]); append(parent, row);
}
fn settings() -> config::Ini { config::Ini::parse(&std::fs::read_to_string(config::settings_path()).unwrap_or_default()) }
fn save_settings(items: &[(&str, String)]) -> Result<(), String> {
    let mut ini = settings();
    for (key, value) in items {
        if value.contains(['\r', '\n']) { return Err("Settings cannot contain line breaks".into()); }
        ini.set("Settings", key, value);
    }
    std::fs::create_dir_all(config::config_dir()).map_err(|e| e.to_string())?;
    let temporary = config::settings_path().with_extension("cfg.native.tmp");
    std::fs::write(&temporary, ini.to_text()).map_err(|e| e.to_string())?;
    std::fs::rename(temporary, config::settings_path()).map_err(|e| e.to_string())
}
fn default_curve() -> Vec<(f32,f32)> { vec![(40.0,30.0), (55.0,55.0), (70.0,85.0), (85.0,100.0)] }
fn parse_curve(s: &str) -> Option<Vec<(f32,f32)>> {
    let nodes: Option<Vec<_>> = s.split(',').map(|pair| { let (a,b) = pair.split_once(':')?; Some((a.trim().parse::<f32>().ok()?, b.trim().parse::<f32>().ok()?)) }).collect();
    let nodes = nodes?; valid_curve(&nodes).then_some(nodes)
}
fn valid_curve(nodes: &[(f32,f32)]) -> bool {
    (2..=32).contains(&nodes.len()) && nodes.iter().all(|(t,d)| t.is_finite() && d.is_finite() && (30.0..=110.0).contains(t) && (0.0..=100.0).contains(d))
        && nodes.windows(2).all(|w| w[0].0 < w[1].0)
}

impl Ui {
    fn status(&self, msg: &str) { text(self.status, msg); }
    fn send(&self, work: Work) { self.status("Applying…"); if self.tx.send(work).is_err() { self.status("The hardware worker is not available"); } }
    fn admin(&self) -> bool { unsafe { gtk_check_button_get_active(self.admin) != 0 } }
    fn disable_curve(&mut self) -> Result<(), String> {
        save_settings(&[("SoftwareFanCurveEnabled", "0".into())])?; self.refresh_curve_status(); Ok(())
    }
    fn act(&mut self, action: Action) {
        match self.perform(action) { Ok(()) => {}, Err(e) => self.status(&format!("Not applied: {e}")) }
    }
    fn perform(&mut self, action: Action) -> Result<(), String> {
        unsafe {
            match action {
                Action::Page(id) => gtk_stack_set_visible_child_name(self.stack, c(id).as_ptr()),
                Action::Power => {
                    let v = gtk_range_get_value(self.power) as f32;
                    let r = self.readings.get(self.selected).ok_or("No GPU selected")?;
                    if !r.caps.power_limit { return Err("The driver does not support power-limit adjustments".into()); }
                    self.send(Work::Apply(self.selected, ApplySettings { power_limit_w: Some(v), ..Default::default() }, self.admin()));
                }
                Action::Offsets => {
                    let r = self.readings.get(self.selected).ok_or("No GPU selected")?;
                    let s = ApplySettings { core_offset_mhz: r.caps.core_clock_offset.then(|| spin_value(self.core) as i32),
                        mem_offset_mhz: r.caps.mem_clock_offset.then(|| spin_value(self.memory) as i32), ..Default::default() };
                    self.send(Work::Apply(self.selected, s, self.admin()));
                }
                Action::Locks => {
                    let mut s = ApplySettings::default();
                    if gtk_check_button_get_active(self.lock_core_on) != 0 { s.lock_core_clock = Some(read_range(self.lock_core)?); }
                    if gtk_check_button_get_active(self.lock_mem_on) != 0 { s.lock_mem_clock = Some(read_range(self.lock_mem)?); }
                    if s.lock_core_clock.is_none() && s.lock_mem_clock.is_none() { return Err("Enable at least one clock range".into()); }
                    self.send(Work::Apply(self.selected, s, self.admin()));
                }
                Action::ManualFan => {
                    self.disable_curve()?;
                    self.manual_mode = true;
                    self.send(Work::Apply(self.selected, ApplySettings { fan_pct: Some(gtk_range_get_value(self.fan) as u32), ..Default::default() }, false));
                }
                Action::AutoFan => {
                    self.disable_curve()?;
                    self.manual_mode = false;
                    self.send(Work::Apply(self.selected, ApplySettings { fan_auto: true, ..Default::default() }, false));
                }
                Action::ConfirmReset => self.confirm("Restore the selected GPU? This requests default power and clocks, disables the active fan curve, and returns fan control to the driver. Some changes require authorization.", Action::Reset),
                Action::Reset => { self.disable_curve()?; self.send(Work::Reset(self.selected, self.admin())); }
                Action::Preset => {
                    self.curve = match gtk_combo_box_get_active(self.preset) {
                        1 => vec![(40.0,30.0), (55.0,40.0), (65.0,55.0), (75.0,80.0), (85.0,100.0)],
                        2 => vec![(35.0,40.0), (50.0,55.0), (65.0,75.0), (75.0,90.0), (85.0,100.0)],
                        _ => default_curve(),
                    };
                    self.rebuild_nodes(0); self.status("Preset loaded in the editor. Click Enable curve to apply it.");
                }
                Action::AddNode => {
                    if self.curve.len() >= 32 { return Err("A curve can contain at most 32 points".into()); }
                    let p = self.curve.windows(2).enumerate().max_by(|(_,a),(_,b)| (a[1].0-a[0].0).total_cmp(&(b[1].0-b[0].0))).map(|(i,_)| i).unwrap_or(0);
                    let a = self.curve[p]; let b = self.curve[p+1];
                    self.curve.insert(p+1, ((a.0+b.0)/2.0, (a.1+b.1)/2.0)); self.rebuild_nodes(p+1);
                }
                Action::UpdateNode => {
                    let i = gtk_combo_box_get_active(self.node_combo).max(0) as usize;
                    if i < self.curve.len() {
                        let t = spin_value(self.node_temp) as f32; let d = spin_value(self.node_duty) as f32;
                        let mut nodes = self.curve.clone(); nodes[i] = (t,d); nodes.sort_by(|a,b| a.0.total_cmp(&b.0));
                        if !valid_curve(&nodes) { return Err("Each point needs a unique temperature between 30 and 110 °C".into()); }
                        self.curve = nodes; let index = self.curve.iter().position(|p| p.0 == t).unwrap_or(0); self.rebuild_nodes(index);
                    }
                }
                Action::RemoveNode => {
                    if self.curve.len() <= 2 { return Err("The curve needs at least two points".into()); }
                    let i = gtk_combo_box_get_active(self.node_combo).max(0) as usize;
                    self.curve.remove(i.min(self.curve.len()-1)); self.rebuild_nodes(i.min(self.curve.len()-1));
                }
                Action::CurveOn => {
                    if !valid_curve(&self.curve) { return Err("Invalid fan curve".into()); }
                    save_settings(&[("SoftwareFanCurve", config::format_nodes(&self.curve)), ("SoftwareFanCurveEnabled", "1".into()), ("SoftwareFanCurveGpu", self.selected.to_string())])?;
                    self.refresh_curve_status(); self.status("Curve saved. A running background service applies it on the next poll.");
                }
                Action::CurveOff => { self.disable_curve()?; self.send(Work::Apply(self.selected, ApplySettings { fan_auto: true, ..Default::default() }, false)); }
                Action::ConfirmSave(s) => {
                    if config::load_profile(s).is_some() { self.confirm(&format!("Overwrite profile {s}?"), Action::SaveProfile(s)); }
                    else { return self.perform(Action::SaveProfile(s)); }
                }
                Action::SaveProfile(s) => { self.save_profile(s)?; self.refresh_profiles(); self.status(&format!("Profile {s} saved.")); }
                Action::LoadProfile(s) => self.load_profile(s)?,
                Action::SaveSettings => {
                    let path = entry_value(self.log_path);
                    if !PathBuf::from(&path).is_absolute() { return Err("The CSV file path must be absolute".into()); }
                    save_settings(&[("EnableLog", if gtk_switch_get_active(self.log_on) != 0 { "1" } else { "0" }.into()),
                        ("LogPath", path), ("HwPollPeriod", (spin_value(self.poll_period) as u64).to_string()),
                        ("EnableServer", if gtk_check_button_get_active(self.api_on) != 0 { "1" } else { "0" }.into())])?;
                    self.status("Settings saved. CSV logging and polling update live; changing the optional web server requires a service restart.");
                }
                Action::OpenConfig => { std::process::Command::new("xdg-open").arg(config::config_dir()).spawn().map_err(|e| e.to_string())?; }
                Action::OpenLog => { std::process::Command::new("xdg-open").arg(entry_value(self.log_path)).spawn().map_err(|e| e.to_string())?; }
                Action::Service(action) => self.send(Work::Service(action)),
            }
        }
        Ok(())
    }

    fn sync_controls(&mut self) {
        let Some(r) = self.readings.get(self.selected) else { return };
        unsafe {
            if let (Some(lo), Some(hi)) = (r.limits.power_min_w, r.limits.power_max_w) {
                if hi > lo { gtk_range_set_range(self.power, lo as f64, hi as f64); }
            }
            gtk_range_set_value(self.power, r.sample.power_limit_w.or(r.limits.power_default_w).unwrap_or(100.0) as f64);
            gtk_range_set_value(self.fan, r.sample.fan_pct.unwrap_or(50.0).max(30.0) as f64);
            text(self.power_hint, &format!("{}–{} W · default {} W", number(r.limits.power_min_w,0), number(r.limits.power_max_w,0), number(r.limits.power_default_w,0)));
            gtk_widget_set_sensitive(self.power_button, r.caps.power_limit as i32);
            gtk_widget_set_sensitive(self.offsets_button, (r.caps.core_clock_offset || r.caps.mem_clock_offset) as i32);
            gtk_widget_set_sensitive(self.fan_button, r.caps.fan as i32); gtk_widget_set_sensitive(self.auto_button, r.caps.fan_auto as i32);
            gtk_widget_set_sensitive(self.core, r.caps.core_clock_offset as i32); gtk_widget_set_sensitive(self.memory, r.caps.mem_clock_offset as i32);
            gtk_widget_queue_draw(self.chart);
        }
    }
    fn render_readings(&mut self) {
        let Some(r) = self.readings.get(self.selected) else { text(self.gpu_title, "No GPUs detected"); return };
        let name = r.gpu.name.as_deref().unwrap_or(&r.gpu.driver);
        text(self.gpu_title, name);
        let s = &r.sample;
        let values = [s.temp_core, s.usage_gpu, s.power_w, s.fan_pct, s.clk_core, s.clk_mem];
        let details = [format!("Memory: {} °C", number(s.temp_mem,0)), format!("Memory: {} %", number(s.usage_mem,0)),
            format!("Limit: {} W", number(s.power_limit_w,0)), format!("{} RPM · driver reading", number(s.fan_rpm,0)),
            "Current frequency".into(), "Current frequency".into()];
        for (i,m) in self.metrics.iter().enumerate() {
            text(m.value, &number(values[i], if i == 2 {1} else {0})); text(m.detail, &details[i]);
            let max = match i { 0 => 100.0, 1|3 => 100.0, 2 => r.limits.power_max_w.unwrap_or(400.0), 4 => 3000.0, _ => 12000.0 };
            unsafe { gtk_progress_bar_set_fraction(m.bar, (values[i].unwrap_or(0.0) / max).clamp(0.0,1.0) as f64); }
        }
        text(self.fan_status, &format!("Actual speed: {} % · {} RPM. Automatic mode may stop the fans while the GPU is cool.", number(s.fan_pct,0), number(s.fan_rpm,0)));
        unsafe { gtk_widget_queue_draw(self.chart); gtk_widget_queue_draw(self.curve_area); }
    }
    fn rebuild_nodes(&mut self, selected: usize) { unsafe {
        gtk_combo_box_text_remove_all(self.node_combo);
        for (i,(t,d)) in self.curve.iter().enumerate() {
            gtk_combo_box_text_append_text(self.node_combo, c(&format!("Point {} · {t:.0} °C → {d:.0} %",i+1)).as_ptr());
        }
        gtk_combo_box_set_active(self.node_combo, selected.min(self.curve.len().saturating_sub(1)) as i32);
        self.sync_node(); gtk_widget_queue_draw(self.curve_area);
    } }
    fn sync_node(&self) { unsafe {
        let i = gtk_combo_box_get_active(self.node_combo).max(0) as usize;
        if let Some((t,d)) = self.curve.get(i) { gtk_spin_button_set_value(self.node_temp, *t as f64); gtk_spin_button_set_value(self.node_duty, *d as f64); }
    } }
    fn refresh_curve_status(&self) {
        let ini = settings();
        text(self.curve_status, if ini.get_i64("Settings", "SoftwareFanCurveEnabled").unwrap_or(0) == 1 {
            "Curve enabled in the settings. On supported NVIDIA GPUs, speeds are clamped to the driver minimum; 0 restores automatic control."
        } else { "Curve disabled. Editing points or loading presets does not change the hardware until you click Enable curve." });
    }
    fn refresh_profiles(&self) {
        for (i,w) in self.profile_labels.iter().enumerate() {
            text(*w, &config::load_profile(i as u32+1).map(|p| format!("{}\nCore {:+} · Mem. {:+}\n{} W · {}",p.name,
                p.core_clock_offset.unwrap_or(0),p.mem_clock_offset.unwrap_or(0),number(p.power_limit_w,0),
                if p.fan_auto { "Automatic fans".into() } else { format!("Fan {} %",p.fan_pct.unwrap_or(0)) })).unwrap_or_else(|| "Not saved\nCustomize the controls and save here.".into()));
        }
    }
    fn save_profile(&self, slot: u32) -> Result<(), String> {
        let r = self.readings.get(self.selected).ok_or("No GPU selected")?;
        let name = entry_value(self.profile_name); let name = if name.trim().is_empty() { format!("Profile {slot}") } else { name.trim().to_string() };
        if name.contains(['\r','\n']) { return Err("Invalid profile name".into()); }
        let curve_on = settings().get_i64("Settings","SoftwareFanCurveEnabled").unwrap_or(0)==1;
        let p = config::ProfileValues { name, core_clock_offset: r.caps.core_clock_offset.then(|| spin_value(self.core) as i32),
            mem_clock_offset: r.caps.mem_clock_offset.then(|| spin_value(self.memory) as i32),
            power_limit_w: r.caps.power_limit.then(|| unsafe { gtk_range_get_value(self.power) as f32 }),
            fan_auto: !self.manual_mode, fan_pct: self.manual_mode.then(|| unsafe {gtk_range_get_value(self.fan) as u32}),
            fan_curve_nodes: self.curve.clone(), ..Default::default() };
        let path = config::save_profile(slot,&p,&r.limits).map_err(|e| e.to_string())?;
        let mut ini = config::Ini::parse(&std::fs::read_to_string(&path).map_err(|e|e.to_string())?);
        ini.set("Settings","SoftwareFanCurveEnabled",if curve_on {"1"} else {"0"});
        // Preserve lock ranges without changing the public Afterburner-compatible profile fields.
        for (name,on,range) in [("LockCore",self.lock_core_on,self.lock_core),("LockMem",self.lock_mem_on,self.lock_mem)] {
            if unsafe { gtk_check_button_get_active(on) }!=0 { let (lo,hi)=read_range(range)?; ini.set("Settings",&format!("{name}Min"),&lo.to_string()); ini.set("Settings",&format!("{name}Max"),&hi.to_string()); }
        }
        std::fs::write(path,ini.to_text()).map_err(|e|e.to_string())
    }
    fn load_profile(&mut self, slot: u32) -> Result<(),String> {
        let p = config::load_profile(slot).ok_or("This profile is empty")?;
        self.manual_mode = !p.fan_auto;
        let path = config::profiles_dir().join(format!("Profile{slot}.cfg"));
        let ini = config::Ini::parse(&std::fs::read_to_string(path).map_err(|e|e.to_string())?);
        let mut s = ApplySettings { core_offset_mhz:p.core_clock_offset,mem_offset_mhz:p.mem_clock_offset,
            power_limit_w:p.power_limit_w.or_else(|| p.power_limit_pct.and_then(|pct| self.readings.get(self.selected)?.limits.power_default_w.map(|w|w*pct/100.0))),
            fan_pct:p.fan_pct,fan_auto:p.fan_auto,..Default::default() };
        for (name,range,target,on) in [("LockCore",self.lock_core,&mut s.lock_core_clock,self.lock_core_on),("LockMem",self.lock_mem,&mut s.lock_mem_clock,self.lock_mem_on)] {
            if let (Some(lo),Some(hi))=(ini.get_i64("Settings",&format!("{name}Min")),ini.get_i64("Settings",&format!("{name}Max"))) {
                if lo>0 && hi>=lo { *target=Some((lo as u32,hi as u32)); unsafe {gtk_spin_button_set_value(range[0],lo as f64);gtk_spin_button_set_value(range[1],hi as f64);gtk_check_button_set_active(on,1);} }
            }
        }
        unsafe { gtk_spin_button_set_value(self.core,p.core_clock_offset.unwrap_or(0) as f64); gtk_spin_button_set_value(self.memory,p.mem_clock_offset.unwrap_or(0) as f64);
            if let Some(w)=s.power_limit_w { gtk_range_set_value(self.power,w as f64); } if let Some(f)=p.fan_pct {gtk_range_set_value(self.fan,f as f64);} }
        if valid_curve(&p.fan_curve_nodes) { self.curve=p.fan_curve_nodes; self.rebuild_nodes(0); }
        self.disable_curve()?;
        self.send(Work::Apply(self.selected,s,self.admin()));
        if ini.get_i64("Settings","SoftwareFanCurveEnabled").unwrap_or(0)==1 { self.perform(Action::CurveOn)?; }
        Ok(())
    }
    fn confirm(&self, message:&str, action:Action) { unsafe {
        let dialog=gtk_message_dialog_new(self.window,1,1,0,c"%s".as_ptr(),c(message).as_ptr());
        gtk_window_set_title(dialog,c"Confirm action".as_ptr());
        gtk_dialog_add_button(dialog,c"Cancel".as_ptr(),-6);
        gtk_dialog_add_button(dialog,if matches!(action,Action::Reset) {c"Restore".as_ptr()} else {c"Save".as_ptr()},-8);
        g_signal_connect_data(dialog,c"response".as_ptr(),Some(callback(confirm_response as *const ())),Box::into_raw(Box::new(action)).cast(),Some(free_action),0);
        gtk_window_present(dialog);
    } }
    fn refresh_sensors(&mut self) {
        let fresh=std::fs::metadata(crate::mahm::SHM_ACCESS_NAME).and_then(|m|m.modified()).ok().and_then(|t|t.elapsed().ok()).is_some_and(|d|d.as_secs()<5);
        text(self.badge,if fresh {"GPU ONLINE"} else {"LOCAL MONITOR"});
        clear_box(self.sensor_table);
        let g=grid(); unsafe {gtk_grid_set_row_spacing(g,0);gtk_grid_set_column_spacing(g,0);} append(self.sensor_table,g);
        for (i,t) in ["Sensor","Value","Unit","GPU","ID"].iter().enumerate() {attach(g,label(t,"table-header"),i as i32,0,1);}
        match crate::mahm::entries_from_shm() {
            Ok(entries) => for (row,e) in entries.iter().take(512).enumerate() {
                let val=if e.data.is_finite() && e.data.abs()<3.4e38 {format!("{:.1}",e.data)} else {"Not available".into()};
                for (col,s) in [e.src_name.clone(),val,e.src_units.clone(),(e.gpu+1).to_string(),format!("0x{:02X}",e.src_id)].iter().enumerate() {
                    let l=label(s,"table-cell"); if col==1 {class(l,"table-value");} if col==0 {expand(l);} attach(g,l,col as i32,row as i32+1,1);
                }
            },
            Err(_) => attach(g,label("The background service is not publishing MAHM yet", "table-cell"),0,1,5),
        }
        let macm=crate::macm::read_shm();
        text(self.ipc_status,&macm.map(|m|format!("MACM · {} GPU(s) · command 0x{:08X} · master GPU {}",m.gpus.len(),m.command,m.master_gpu+1))
            .unwrap_or_else(||"MACM · No client connected. Experimental control-memory support is provided by the service.".into()));
    }
}

fn number(v:Option<f32>,digits:usize)->String {v.filter(|v|v.is_finite()&&v.abs()<3.4e38).map(|v|format!("{v:.digits$}")).unwrap_or_else(||"—".into())}
fn read_range(w:[Ptr;2])->Result<(u32,u32),String> {let a=spin_value(w[0]) as u32;let b=spin_value(w[1]) as u32;if a==0||b<a {Err("Clock ranges need a minimum above zero and a maximum at least as large".into())} else {Ok((a,b))}}
unsafe extern "C" fn confirm_response(dialog:Ptr,response:i32,data:Ptr) {
    guarded(|| {let a=(&*data.cast::<Action>()).clone();gtk_window_destroy(dialog);if response==-8 {with_ui(|ui|ui.act(a));}});
}

unsafe extern "C" fn tick(_:Ptr)->i32 {
    // Exercise the real native clicked signal outside an outstanding Ui borrow.
    let test_button = UI.with(|slot| slot.borrow().as_ref().and_then(|ui|
        ui.smoke.as_ref().filter(|s|s.tick==29).map(|_|ui.preset_button)));
    if let Some(w) = test_button { g_signal_emit_by_name(w,c"clicked".as_ptr()); }
    guarded(|| with_ui(|ui| {
        while let Ok(event)=ui.rx.try_recv() {
            match event {
                Event::Finished(msg,error)=>ui.status(&format!("{}{}",if error {"Could not complete the operation: "} else {""},msg)),
                Event::Readings(readings)=>{
                    if !ui.initialized || readings.len()!=ui.readings.len() {
                        gtk_combo_box_text_remove_all(ui.gpu_combo);
                        for (i,r) in readings.iter().enumerate() {gtk_combo_box_text_append_text(ui.gpu_combo,c(&format!("GPU {} · {}",i+1,r.gpu.vendor_name())).as_ptr());}
                        gtk_combo_box_set_active(ui.gpu_combo,0);ui.selected=0;
                        ui.history=vec![VecDeque::new();readings.len()];
                        ui.readings=readings;ui.sync_controls();ui.initialized=true;
                    } else {ui.readings=readings;}
                    for (i,r) in ui.readings.iter().enumerate() {let h=&mut ui.history[i];h.push_back((r.sample.temp_core.unwrap_or(f32::NAN),r.sample.usage_gpu.unwrap_or(f32::NAN)));if h.len()>180 {h.pop_front();}}
                    ui.render_readings();
                }
            }
        }
        ui.ticks+=1;
        if ui.ticks%20==0 {ui.refresh_sensors();ui.refresh_curve_status();}
        smoke_step(ui);
    }));1
}

// Only these charts are custom drawn. All surrounding labels/actions are GTK.
unsafe extern "C" fn draw(_:Ptr,cr:Ptr,width:i32,height:i32,data:Ptr) {
    guarded(|| with_ui(|ui| {
        let w=width as f64;let h=height as f64;
        if w<60.0||h<50.0 {return;}
        chart_grid(cr,w,h);
        if data.is_null() {
            if let Some(history)=ui.history.get(ui.selected) {
                for series in 0..2 {
                    let pts:Vec<_>=history.iter().enumerate().filter_map(|(i,p)|{let v=if series==0 {p.0}else{p.1};v.is_finite().then_some((38.0+i as f64*(w-52.0)/179.0,h-24.0-v as f64/100.0*(h-36.0)))}).collect();
                    neon(cr,&pts,if series==0 {(0.15,0.9,1.0)}else{(1.0,0.62,0.25)});
                }
            }
        } else {
            ui.curve_size=(w,h);
            let pts:Vec<_>=ui.curve.iter().map(|p|point(*p,w,h)).collect();neon(cr,&pts,(0.15,0.9,1.0));
            for (i,(x,y)) in pts.iter().enumerate() {
                cairo_set_source_rgba(cr,1.0,0.65,0.25,0.20);cairo_arc(cr,*x,*y,9.0,0.0,std::f64::consts::TAU);cairo_fill(cr);
                cairo_set_source_rgb(cr,1.0,0.72,0.4);cairo_arc(cr,*x,*y,4.0,0.0,std::f64::consts::TAU);cairo_fill(cr);
                cairo_set_font_size(cr,10.0);cairo_move_to(cr,*x-10.0,(*y-12.0).max(12.0));cairo_show_text(cr,c(&(i+1).to_string()).as_ptr());
            }
            cairo_set_source_rgb(cr,0.55,0.67,0.75);cairo_set_font_size(cr,10.0);
            for t in [40.0,60.0,80.0,100.0] {let (x,_)=point((t,0.0),w,h);cairo_move_to(cr,x-7.0,h-5.0);cairo_show_text(cr,c(&format!("{t:.0}°")).as_ptr());}
        }
    }));
}
unsafe fn chart_grid(cr:Ptr,w:f64,h:f64) {
    cairo_select_font_face(cr,c"Sans".as_ptr(),0,0);cairo_set_font_size(cr,10.0);
    cairo_set_line_width(cr,1.0);cairo_set_source_rgba(cr,0.15,0.6,0.7,0.15);
    for i in 0..=4 {let y=12.0+i as f64*(h-36.0)/4.0;cairo_move_to(cr,38.0,y);cairo_line_to(cr,w-14.0,y);}
    for i in 0..=8 {let x=38.0+i as f64*(w-52.0)/8.0;cairo_move_to(cr,x,12.0);cairo_line_to(cr,x,h-24.0);}
    cairo_stroke(cr);cairo_set_source_rgb(cr,0.45,0.6,0.68);
    for (p,y) in [("100",18.0),("50",(h-12.0)/2.0),("0",h-24.0)] {cairo_move_to(cr,5.0,y);cairo_show_text(cr,c(p).as_ptr());}
}
unsafe fn neon(cr:Ptr,pts:&[(f64,f64)],rgb:(f64,f64,f64)) {
    if pts.len()<2 {return;}
    for (width,alpha) in [(6.0,0.12),(2.0,1.0)] {
        cairo_set_line_width(cr,width);cairo_set_source_rgba(cr,rgb.0,rgb.1,rgb.2,alpha);
        cairo_move_to(cr,pts[0].0,pts[0].1);for (x,y) in &pts[1..] {cairo_line_to(cr,*x,*y);}cairo_stroke(cr);
    }
}
fn point((t,d):(f32,f32),w:f64,h:f64)->(f64,f64) {(38.0+(t as f64-30.0)/80.0*(w-52.0),h-24.0-d as f64/100.0*(h-36.0))}
fn unpoint(x:f64,y:f64,w:f64,h:f64)->(f32,f32) {(((x-38.0)/(w-52.0).max(1.0)*80.0+30.0).clamp(30.0,110.0) as f32,((h-24.0-y)/(h-36.0).max(1.0)*100.0).clamp(0.0,100.0) as f32)}
fn hit(ui:&Ui,x:f64,y:f64)->Option<usize> {ui.curve.iter().position(|p|{let(a,b)=point(*p,ui.curve_size.0,ui.curve_size.1);(a-x).powi(2)+(b-y).powi(2)<121.0})}
unsafe extern "C" fn curve_press(_:Ptr,n:i32,x:f64,y:f64,_:Ptr) {
    guarded(||with_ui(|ui| {
        if let Some(i)=hit(ui,x,y) {
            if n==2 && ui.curve.len()>2 {ui.curve.remove(i);ui.drag=None;ui.rebuild_nodes(0);}else{ui.drag=Some(i);gtk_combo_box_set_active(ui.node_combo,i as i32);ui.sync_node();}
        }
    }));
}
unsafe extern "C" fn curve_motion(_:Ptr,x:f64,y:f64,_:Ptr) {
    guarded(||with_ui(|ui| {if let Some(i)=ui.drag {
        let(mut t,d)=unpoint(x,y,ui.curve_size.0,ui.curve_size.1);
        let lo=if i==0 {30.0}else{ui.curve[i-1].0+0.5};let hi=if i+1==ui.curve.len() {110.0}else{ui.curve[i+1].0-0.5};
        if lo<=hi {t=t.clamp(lo,hi);ui.curve[i]=(t,d);ui.sync_node();gtk_widget_queue_draw(ui.curve_area);}
    }}));
}
unsafe extern "C" fn curve_release(_:Ptr,_:i32,_:f64,_:f64,_:Ptr) {
    guarded(||with_ui(|ui|{let i=ui.drag.take().unwrap_or(0);ui.rebuild_nodes(i);}));
}

// UI smoke test: visit every page, exercise harmless native controls and save only this window.
fn smoke_step(ui:&mut Ui) {
    let Some(mut test)=ui.smoke.take() else {return;};test.tick+=1;
    if test.tick==30 {
        ui.act(Action::AddNode);ui.act(Action::UpdateNode);ui.act(Action::RemoveNode);
        unsafe {gtk_range_set_value(ui.fan,57.0);gtk_stack_set_visible_child_name(ui.stack,c"monitor".as_ptr());}
    }
    if test.tick>=40 && (test.tick-40)%8==0 && test.page<PAGES.len() {
        unsafe {gtk_stack_set_visible_child_name(ui.stack,c(PAGES[test.page].0).as_ptr());}
    }
    if test.tick>=44 && (test.tick-44)%8==0 && test.page<PAGES.len() {
        let _=std::fs::create_dir_all(&test.dir);
        let path=test.dir.join(format!("{}.png",PAGES[test.page].0));
        if unsafe {snapshot(ui.window,&path)} {test.captures+=1;test.page+=1;}
        else { eprintln!("Snapshot pending: {}",PAGES[test.page].0); }
    }
    if test.page==PAGES.len() {
        let counts=unsafe {widget_counts(ui.window)};
        let ok=test.captures==PAGES.len() && unsafe {gtk_range_get_value(ui.fan)} == 57.0 && counts.get("GtkButton").copied().unwrap_or(0)>=20
            && counts.get("GtkScale").copied().unwrap_or(0)>=2 && !counts.keys().any(|n|n.contains("WebView")||n.contains("Terminal"));
        let report=format!("GTK_SMOKE_{} pages={} native_widgets={counts:?}\n",if ok {"OK"}else{"FAILED"},test.captures);
        println!("{report}");let _=std::fs::write(test.dir.join("report.txt"),report);
        let _=ui.tx.send(Work::Stop);
        unsafe {if ui.source!=0 {g_source_remove(ui.source);ui.source=0;}g_timeout_add(100,Some(smoke_close),ui.window);}
        return;
    }
    ui.smoke=Some(test);
}
unsafe extern "C" fn smoke_close(window:Ptr)->i32 {gtk_window_close(window);0}
unsafe fn snapshot(window:Ptr,path:&std::path::Path)->bool {
    let paint=gtk_widget_paintable_new(window);let snap=gtk_snapshot_new();
    gdk_paintable_snapshot(paint,snap,gtk_widget_get_width(window) as f64,gtk_widget_get_height(window) as f64);
    let node=gtk_snapshot_free_to_node(snap);g_object_unref(paint);if node.is_null(){return false;}
    let renderer=gtk_native_get_renderer(gtk_widget_get_native(window));
    let texture=gsk_renderer_render_texture(renderer,node,std::ptr::null_mut());gsk_render_node_unref(node);if texture.is_null(){return false;}
    let ok=gdk_texture_save_to_png(texture,c(&path.to_string_lossy()).as_ptr())!=0;g_object_unref(texture);ok
}
unsafe fn widget_counts(w:Ptr)->BTreeMap<String,usize> {
    let name=CStr::from_ptr(g_type_name_from_instance(w)).to_string_lossy().into_owned();let mut counts=BTreeMap::from([(name,1)]);
    let mut child=gtk_widget_get_first_child(w);while !child.is_null(){for(name,n)in widget_counts(child){*counts.entry(name).or_default()+=n;}child=gtk_widget_get_next_sibling(child);}counts
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn curves_are_bounded_finite_and_strictly_ordered() {
        assert!(valid_curve(&default_curve()));assert!(parse_curve("40:30,55:55,70:85,85:100").is_some());
        for c in ["40:30,40:80","40:30,30:90","40:NaN,80:100","40:30,85:101","40:30"] {assert!(parse_curve(c).is_none());}
    }
    #[test] fn curve_coordinates_round_trip() {let p=(65.0,55.0);let(x,y)=point(p,600.0,300.0);let q=unpoint(x,y,600.0,300.0);assert!((p.0-q.0).abs()<0.001&&(p.1-q.1).abs()<0.001);}
}
