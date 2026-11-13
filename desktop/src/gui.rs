//! Desktop window (egui): the same tabs as the Android app, talking to the yggproxy service.

use crate::api::{self, dur, human, limit_text, mbit};
use crate::config::{self, Config};
use eframe::egui;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Data {
    status: Option<Value>,
    error: Option<String>,
    logs: Vec<String>,
    log_seq: u64,
    toast: Option<(String, Instant)>,
    speed: Vec<String>,
    speed_running: bool,
    /// Server: keys and requests (`{"cmd":"access"}`).
    access: Option<Value>,
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Client,
    Server,
    Network,
    Status,
    Logs,
}

struct App {
    data: Arc<Mutex<Data>>,
    tab: Tab,
    /// Editable copy of the settings (loaded from the service).
    cfg: Option<Config>,
    peers_text: String,
    lim: [String; 4],
    qr: Option<(String, egui::TextureHandle)>,
    peer_detail: Option<Value>,
    /// Client tab: address (or link) and key being typed.
    add_text: String,
    key_text: String,
    /// Client tab: message for an access request.
    req_msg: String,
    /// Server tab: new key's name and days, days for approvals; the key whose QR is shown.
    new_name: String,
    new_days: u32,
    qr_key: Option<String>,
    /// The risk dialog before making the server public (and its checkbox).
    public_dialog: Option<bool>,
    /// Network tab: tracker list being edited, and the list it was loaded from.
    trackers_text: String,
    trackers_loaded: Vec<String>,
    /// YGGPROXY_SHOT=DIR (development aid): screenshot every tab into DIR/*.ppm, then quit.
    shot: Option<(String, usize, Instant)>,
}

fn poller(data: Arc<Mutex<Data>>, ctx: egui::Context) {
    loop {
        let t0 = std::time::Instant::now();
        let st = api::status();
        let since = data.lock().unwrap().log_seq;
        let logs = api::request(json!({"cmd": "logs", "since": since}));
        let access = api::request(json!({"cmd": "access"})).ok();
        {
            let mut d = data.lock().unwrap();
            match st {
                Ok(s) => {
                    d.status = Some(s);
                    d.error = None;
                }
                Err(e) => {
                    d.status = None;
                    d.error = Some(e);
                }
            }
            d.access = access;
            if let Ok(l) = logs {
                for x in l["lines"].as_array().into_iter().flatten() {
                    d.logs.push(x.as_str().unwrap_or("").to_string());
                }
                if d.logs.len() > 3000 {
                    let n = d.logs.len() - 3000;
                    d.logs.drain(..n);
                }
                d.log_seq = l["seq"].as_u64().unwrap_or(d.log_seq);
            }
        }
        ctx.request_repaint();
        std::thread::sleep(Duration::from_millis(1000).saturating_sub(t0.elapsed()));
    }
}

fn mbit_field(kbit: u64) -> String {
    if kbit.is_multiple_of(1000) { (kbit / 1000).to_string() } else { format!("{:.1}", kbit as f64 / 1000.0) }
}

fn parse_mbit(s: &str) -> u64 {
    (s.trim().replace(',', ".").parse::<f64>().unwrap_or(0.0).max(0.0) * 1000.0).round() as u64
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        let data = Arc::new(Mutex::new(Data::default()));
        let (d, ctx) = (data.clone(), cc.egui_ctx.clone());
        std::thread::spawn(move || poller(d, ctx));
        let mut app = App { data, tab: Tab::Client, cfg: None, peers_text: String::new(), lim: Default::default(), qr: None, peer_detail: None, add_text: String::new(), key_text: String::new(), req_msg: String::new(), new_name: String::new(), new_days: 30, qr_key: None, public_dialog: None, trackers_text: String::new(), trackers_loaded: Vec::new(),
            shot: std::env::var("YGGPROXY_SHOT").ok().map(|d| (d, 0, Instant::now())) };
        app.reload_config();
        app
    }

    fn reload_config(&mut self) {
        if let Ok(c) = api::config() {
            self.peers_text = c.network.peers.join("\n");
            self.trackers_text = c.network.trackers.join("\n");
            self.trackers_loaded = c.network.trackers.clone();
            let l = c.server.limits;
            self.lim = [l.total_down, l.total_up, l.client_down, l.client_up].map(mbit_field);
            if c.server.enabled || c.client.enabled {
                self.tab = Tab::Status;
            }
            self.cfg = Some(c);
        }
    }

    fn toast(&self, msg: impl Into<String>) {
        self.data.lock().unwrap().toast = Some((msg.into(), Instant::now()));
    }

    /// Send the edited settings to the service (off the UI thread).
    fn apply(&mut self, msg: &'static str) {
        let Some(c) = self.cfg.clone() else { return };
        let data = self.data.clone();
        std::thread::spawn(move || {
            let t = match api::set(&c) {
                Ok(()) => msg.to_string(),
                Err(e) => format!("Ошибка: {e}"),
            };
            data.lock().unwrap().toast = Some((t, Instant::now()));
        });
    }

    fn client_tab(&mut self, ui: &mut egui::Ui, st: Option<&Value>) {
        ui.heading("Клиент: интернет через Yggdrasil");
        ui.label("Весь трафик компьютера идёт через туннель и Yggdrasil на сервер выхода.");
        if self.cfg.is_none() {
            return;
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.add_text).desired_width(330.0).hint_text("адрес сервера 200:… или ссылка yggproxy://c?a=…&k=…"));
            ui.add(egui::TextEdit::singleline(&mut self.key_text).desired_width(170.0).hint_text("ключ доступа"));
            if ui.button("Добавить").clicked() {
                match config::parse_server(&self.add_text) {
                    Some(a) => {
                        let key = config::link_key(&self.add_text).unwrap_or_else(|| crate::access::norm_secret(&self.key_text));
                        let c = self.cfg.as_mut().unwrap();
                        if !c.client.servers.contains(&a.to_string()) {
                            c.client.servers.push(a.to_string());
                        }
                        if !key.is_empty() {
                            c.client.keys.insert(a.to_string(), key);
                        }
                        self.key_text.clear();
                        if config::parse_server(&c.client.server).is_none() {
                            c.client.server = a.to_string();
                        }
                        self.add_text.clear();
                        self.apply("Сервер добавлен");
                    }
                    None => self.toast("Адрес сервера: IPv6 из 200::/7, например 200:…"),
                }
            }
        });

        // Saved servers and the trackers' exits, with the node's ping.
        ui.add_space(4.0);
        ui.label("Серверы (нажмите — подключиться). Пинг меряет узел Yggdrasil каждые 10 с.");
        ui.label("Серверы закрыты: нужен ключ от владельца. Без ключа при подключении ему уйдёт запрос доступа. 🔑 — ключ есть, 🌐 — публичный.");
        let keys = self.cfg.as_ref().unwrap().client.keys.clone();
        let public: std::collections::HashSet<String> = st
            .map(|s| s["trackers"]["exits"].as_array().into_iter().flatten().filter(|e| e["public"].as_bool() == Some(true)).filter_map(|e| e["address"].as_str().map(String::from)).collect())
            .unwrap_or_default();
        let ping = st.map(api::ping_map).unwrap_or_default();
        let saved = self.cfg.as_ref().unwrap().client.servers.clone();
        let tracker: Vec<String> = st
            .map(|s| s["trackers"]["exits"].as_array().into_iter().flatten().filter_map(|e| e["address"].as_str().map(String::from)).filter(|a| !saved.contains(a)).collect())
            .unwrap_or_default();
        let (cur, on, active) = {
            let c = self.cfg.as_ref().unwrap();
            (c.client.server.clone(), c.client.enabled, st.map(|s| s["client"]["active"].as_bool() == Some(true)).unwrap_or(false))
        };
        let mut connect: Option<String> = None;
        let mut forget: Option<String> = None;
        let mut row = |ui: &mut egui::Ui, a: &String, is_saved: bool| {
            let p = ping.get(a).map(|r| format!("{r} мс")).unwrap_or("нет ответа".into());
            let mark = if *a == cur && on && active { "● " } else if *a == cur && on { "◐ " } else if *a == cur { "○ " } else { "  " };
            let kind = if keys.contains_key(a) { "🔑" } else if public.contains(a) { "🌐" } else { "  " };
            ui.horizontal(|ui| {
                let txt = egui::RichText::new(format!("{mark}{kind} {a}   {p}")).monospace();
                if ui.add(egui::Button::new(if *a == cur { txt.strong() } else { txt }).frame(false)).clicked() {
                    connect = Some(a.clone());
                }
                if is_saved && ui.small_button("убрать").clicked() {
                    forget = Some(a.clone());
                }
            });
        };
        if saved.is_empty() && tracker.is_empty() {
            ui.label("Список пуст: добавьте адрес или дождитесь списка от трекеров.");
        }
        for a in &saved {
            row(ui, a, true);
        }
        if !tracker.is_empty() {
            ui.label(egui::RichText::new("От трекеров").strong());
            for a in &tracker {
                row(ui, a, false);
            }
        }
        if let Some(s) = st {
            let root = &s["rtt"]["root"];
            ui.monospace(format!("Корень сети: {}", root["rtt_ms"].as_u64().map(|r| format!("{r} мс")).unwrap_or("нет ответа".into())));
            if let Some(t) = s["trackers"]["status"].as_str() {
                ui.label(format!("Трекеры: {t}"));
            }
        }
        if let Some(a) = forget {
            let c = self.cfg.as_mut().unwrap();
            c.client.servers.retain(|x| *x != a);
            self.apply("Убран из списка");
        }
        if let Some(a) = connect {
            let c = self.cfg.as_mut().unwrap();
            if !c.client.servers.contains(&a) {
                c.client.servers.push(a.clone());
            }
            c.client.server = a;
            c.client.enabled = true;
            c.client.ygg_only = false;
            self.apply("Подключаюсь…");
        }

        ui.add_space(6.0);
        let c = self.cfg.as_mut().unwrap();
        let on = c.client.enabled;
        if ui.add_sized([220.0, 32.0], egui::Button::new(if on { "Отключить VPN" } else { "Подключить VPN" })).clicked() {
            if on {
                c.client.enabled = false;
                self.apply("VPN выключен");
            } else if c.client.ygg_only {
                c.client.enabled = true;
                self.apply("VPN только для Yggdrasil");
            } else {
                // The selected server, else the best by ping, else the first in the list.
                let pick = config::parse_server(&c.client.server).map(|a| a.to_string())
                    .or_else(|| saved.iter().chain(tracker.iter()).filter(|a| ping.contains_key(*a)).min_by_key(|a| ping[*a]).cloned())
                    .or_else(|| saved.first().or(tracker.first()).cloned());
                match pick {
                    Some(a) => {
                        if !c.client.servers.contains(&a) {
                            c.client.servers.push(a.clone());
                        }
                        c.client.server = a;
                        c.client.enabled = true;
                        self.apply("Подключаюсь…");
                    }
                    None => self.toast("Добавьте сервер или дождитесь списка от трекеров"),
                }
            }
        }
        let c = self.cfg.as_mut().unwrap();
        ui.checkbox(&mut c.client.udp, "UDP через udpgw (QUIC, игры, звонки)");
        ui.checkbox(&mut c.client.ipv6, "IPv6-сайты через сервер");
        let (ys, yo) = (c.client.ygg_sites, c.client.ygg_only);
        ui.checkbox(&mut c.client.ygg_sites, "Сайты Yggdrasil и ALFIS (.ygg, .anon…) — через свой узел, рядом с интернетом")
            .on_hover_text("Адреса 200::/7 и имена ALFIS идут в сеть Yggdrasil напрямую, остальное — через сервер выхода. Только TCP: сайты, SSH, чаты.");
        ui.checkbox(&mut c.client.ygg_only, "Только Yggdrasil: VPN без сервера выхода (интернет как обычно)");
        if (ys, yo) != (c.client.ygg_sites, c.client.ygg_only) {
            self.apply("Сохранено");
        }
        if let Some(s) = st {
            let cl = &s["client"];
            ui.add_space(6.0);
            ui.monospace(if cl["enabled"].as_bool() != Some(true) {
                "VPN выключен".to_string()
            } else if cl["ygg_only"].as_bool() == Some(true) {
                format!("VPN {}: только сайты Yggdrasil и ALFIS\n↓ {}  ↑ {}",
                    if cl["active"].as_bool() == Some(true) { "работает" } else { "запускается…" },
                    human(cl["rx"].as_u64().unwrap_or(0)), human(cl["tx"].as_u64().unwrap_or(0)))
            } else {
                format!(
                    "VPN {} → {}\n↓ {}  ↑ {}\nпинг до сервера: {}\nдоступ: {}",
                    if cl["active"].as_bool() == Some(true) { "работает" } else { "запускается…" },
                    cl["server"].as_str().unwrap_or(""),
                    human(cl["rx"].as_u64().unwrap_or(0)),
                    human(cl["tx"].as_u64().unwrap_or(0)),
                    cl["server_rtt"].as_str().unwrap_or("измеряю…"),
                    match cl["access"].as_str() {
                        Some(e) if e.starts_with("error: ") => format!("не проверен ({})", &e[7..]),
                        Some(code) => format!("{}{}", crate::daemon::access_text(code), api::key_left(cl["access_expires"].as_u64().unwrap_or(0))),
                        None => "проверяю…".into(),
                    }
                )
            });
        }
        // Access request to the selected server (no key): a message for its owner.
        let cur = self.cfg.as_ref().map(|c| c.client.server.clone()).unwrap_or_default();
        if config::parse_server(&cur).is_some() && !self.cfg.as_ref().unwrap().client.keys.contains_key(&cur) {
            ui.add_space(6.0);
            ui.label(format!("Нет ключа к {cur}? Попросите доступ у владельца:"));
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.req_msg).desired_width(330.0).hint_text("сообщение владельцу (кто вы, зачем)"));
                if ui.button("Запросить доступ").clicked() {
                    let (data, server, msg) = (self.data.clone(), cur.clone(), self.req_msg.clone());
                    self.req_msg.clear();
                    std::thread::spawn(move || {
                        let t = match api::request(json!({"cmd": "request_access", "server": server, "message": msg})) {
                            Ok(v) if v["error"].is_null() => v["text"].as_str().unwrap_or("").to_string(),
                            Ok(v) => format!("Ошибка: {}", v["error"].as_str().unwrap_or("?")),
                            Err(e) => format!("Ошибка: {e}"),
                        };
                        data.lock().unwrap().toast = Some((t, Instant::now()));
                    });
                }
            });
        }
        ui.separator();
        ui.heading("Тест скорости до сервера (iperf)");
        let running = self.data.lock().unwrap().speed_running;
        if ui.add_enabled(!running, egui::Button::new("Запустить тест (10 с приём, 10 с отдача)")).clicked() {
            let data = self.data.clone();
            {
                let mut d = data.lock().unwrap();
                d.speed_running = true;
                d.speed.clear();
            }
            std::thread::spawn(move || {
                for down in [true, false] {
                    let dir = if down { "↓ приём" } else { "↑ отдача" };
                    let idx = {
                        let mut d = data.lock().unwrap();
                        d.speed.push(format!("{dir}: …"));
                        d.speed.len() - 1
                    };
                    let d2 = data.clone();
                    let mut prog = move |v: f64| d2.lock().unwrap().speed[idx] = format!("{dir}: {v:.1} Мбит/с …");
                    let r = api::speed_test(down, 10, &mut prog).unwrap_or_else(|e| format!("ошибка: {e}"));
                    data.lock().unwrap().speed[idx] = format!("{dir}: {r}");
                }
                data.lock().unwrap().speed_running = false;
            });
        }
        for l in self.data.lock().unwrap().speed.iter() {
            ui.monospace(l);
        }
    }

    /// A QR code texture for `link` (cached while the link stays the same).
    fn qr_texture(&mut self, ctx: &egui::Context, link: &str) {
        if self.qr.as_ref().map(|q| q.0.as_str()) == Some(link) {
            return;
        }
        if let Some((m, w)) = api::qr(link) {
            let q = 2;
            let side = w + 2 * q;
            let mut px = vec![255u8; side * side];
            for y in 0..w {
                for x in 0..w {
                    if m[y * w + x] {
                        px[(y + q) * side + x + q] = 0;
                    }
                }
            }
            let img = egui::ColorImage::from_gray([side, side], &px);
            self.qr = Some((link.to_string(), ctx.load_texture("qr", img, egui::TextureOptions::NEAREST)));
        }
    }

    /// An access command to the service, off the UI thread; `msg` is shown when it worked.
    fn access_op(&self, req: Value, msg: String) {
        let data = self.data.clone();
        std::thread::spawn(move || {
            let r = api::request(req);
            let mut d = data.lock().unwrap();
            match r {
                Ok(v) if v["error"].is_null() => {
                    d.toast = Some((msg, Instant::now()));
                    d.access = Some(v);
                }
                Ok(v) => d.toast = Some((format!("Ошибка: {}", v["error"].as_str().unwrap_or("?")), Instant::now())),
                Err(e) => d.toast = Some((format!("Ошибка: {e}"), Instant::now())),
            }
        });
    }

    fn server_tab(&mut self, ui: &mut egui::Ui, st: Option<&Value>) {
        ui.heading("Сервер выхода");
        ui.label(format!(
            "Компьютер раздаёт свой интернет через Yggdrasil (порт {}). Сервер закрыт: пользоваться им могут только устройства с ключом — у каждого устройства свой. Локальные сети закрыты.",
            config::PORT
        ));
        let addr = st.and_then(|s| s["node"]["address"].as_str()).map(String::from);
        match &addr {
            Some(a) => {
                ui.monospace(format!("Адрес сервера: {a}"));
                if ui.button("Копировать адрес").clicked() {
                    ui.ctx().copy_text(a.clone());
                    self.toast("Скопировано");
                }
            }
            None => {
                ui.monospace("Адрес появится после запуска узла.");
            }
        }
        self.access_section(ui);
        ui.separator();
        let Some(c) = self.cfg.as_mut() else { return };
        ui.checkbox(&mut c.server.allow_lan, "Разрешить клиентам доступ в мою LAN");
        let on = c.server.enabled;
        if ui.add_sized([220.0, 32.0], egui::Button::new(if on { "Остановить сервер" } else { "Запустить сервер" })).clicked() {
            c.server.enabled = !on;
            self.apply(if on { "Сервер остановлен" } else { "Сервер запускается" });
        }
        if let Some(s) = st.filter(|_| on) {
            let sv = &s["server"];
            ui.monospace(format!(
                "{} · соединений {} (всего {}, отказов {})\nотдано {} · принято {}",
                if sv["active"].as_bool() == Some(true) { "Работает" } else { "Запускается…" },
                sv["active_conns"],
                sv["total"],
                sv["denied"],
                human(sv["tx"].as_u64().unwrap_or(0)),
                human(sv["rx"].as_u64().unwrap_or(0))
            ));
        }
        ui.separator();
        ui.heading("Ограничение скорости, Мбит/с (0 — без ограничения)");
        egui::Grid::new("limits").num_columns(3).show(ui, |ui| {
            ui.label("");
            ui.label("к клиентам");
            ui.label("от клиентов");
            ui.end_row();
            ui.label("Весь сервер");
            ui.add(egui::TextEdit::singleline(&mut self.lim[0]).desired_width(80.0));
            ui.add(egui::TextEdit::singleline(&mut self.lim[1]).desired_width(80.0));
            ui.end_row();
            ui.label("Каждый клиент");
            ui.add(egui::TextEdit::singleline(&mut self.lim[2]).desired_width(80.0));
            ui.add(egui::TextEdit::singleline(&mut self.lim[3]).desired_width(80.0));
            ui.end_row();
        });
        if ui.button("Применить ограничения").clicked() {
            if let Some(c) = self.cfg.as_mut() {
                let l = &mut c.server.limits;
                [l.total_down, l.total_up, l.client_down, l.client_up] = [0, 1, 2, 3].map(|i| parse_mbit(&self.lim[i]));
            }
            self.apply("Ограничения применены (без обрыва соединений)");
        }
        ui.separator();
        ui.heading("Клиенты");
        let list = st.and_then(|s| s["server"]["clients"].as_array().cloned()).unwrap_or_default();
        if list.is_empty() {
            ui.label("нет подключённых клиентов");
        }
        for cl in list {
            ui.monospace(format!(
                "{}\n  соединений {} · отдано ↓ {} · принято ↑ {}",
                cl["address"].as_str().unwrap_or(""),
                cl["active"],
                human(cl["tx"].as_u64().unwrap_or(0)),
                human(cl["rx"].as_u64().unwrap_or(0))
            ));
        }
    }

    fn access_section(&mut self, ui: &mut egui::Ui) {
        let Some(acc) = self.data.lock().unwrap().access.clone() else { return };
        let now = acc["now"].as_u64().unwrap_or(0);
        ui.separator();
        ui.heading("Доступ");
        if acc["public"].as_bool() == Some(true) {
            ui.colored_label(egui::Color32::from_rgb(210, 90, 30), "Сервер ПУБЛИЧНЫЙ: пользоваться им может любой, кто его найдёт.");
            if ui.button("Закрыть сервер (только по ключам)").clicked() {
                self.access_op(json!({"cmd": "access", "op": "public", "on": false}), "Сервер закрыт".into());
            }
        } else {
            ui.horizontal(|ui| {
                ui.label("Сервер закрыт — только по ключам.");
                if ui.small_button("Сделать публичным…").clicked() {
                    self.public_dialog = Some(false);
                }
            });
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Новый ключ для");
            ui.add(egui::TextEdit::singleline(&mut self.new_name).desired_width(150.0).hint_text("чьё устройство"));
            ui.label("на");
            ui.add(egui::DragValue::new(&mut self.new_days).range(1..=365).suffix(" дн."));
            if ui.button("Создать").clicked() {
                self.access_op(json!({"cmd": "access", "op": "new", "name": self.new_name, "days": self.new_days}), "Ключ создан — покажите QR-код устройству".into());
                self.new_name.clear();
                self.qr_key = Some(String::new()); // the newest key
            }
        });
        let keys = acc["keys"].as_array().cloned().unwrap_or_default();
        if self.qr_key.as_deref() == Some("") {
            if let Some(id) = keys.iter().filter(|k| k["addr"].is_null()).max_by_key(|k| k["created"].as_u64()).and_then(|k| k["id"].as_str()) {
                self.qr_key = Some(id.to_string());
            }
        }
        if let Some(k) = keys.iter().find(|k| k["id"].as_str() == self.qr_key.as_deref()).cloned()
            && let Some(link) = k["link"].as_str() {
                self.qr_texture(ui.ctx(), link);
                ui.group(|ui| {
                    ui.label(format!("Ключ «{}» — отсканируйте QR в приложении Ratatoskr или введите адрес и ключ вручную:", k["name"].as_str().unwrap_or("")));
                    if let Some((_, tex)) = &self.qr {
                        ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(220.0, 220.0)));
                    }
                    ui.monospace(format!("Ключ: {}", k["key"].as_str().unwrap_or("")));
                    ui.horizontal(|ui| {
                        if ui.button("Копировать ссылку").clicked() {
                            ui.ctx().copy_text(link.to_string());
                            self.toast("Ссылка скопирована");
                        }
                        if ui.button("Копировать ключ").clicked() {
                            ui.ctx().copy_text(k["key"].as_str().unwrap_or("").to_string());
                            self.toast("Ключ скопирован");
                        }
                        if ui.button("Скрыть").clicked() {
                            self.qr_key = None;
                        }
                    });
                });
            }
        if keys.is_empty() {
            ui.label("Ключей пока нет.");
        }
        for k in &keys {
            let id = k["id"].as_str().unwrap_or("").to_string();
            let exp = k["expires"].as_u64().unwrap_or(0);
            let left = if exp > now { format!("ещё {}", dur(exp - now)) } else { "ИСТЁК".into() };
            let dev = match k["addr"].as_str() {
                Some(a) => format!("{} {a}", k["device"].as_str().unwrap_or("")),
                None => "ещё не использован".into(),
            };
            ui.horizontal_wrapped(|ui| {
                ui.monospace(format!("{} · {} · {} дн., {} · {}", id, k["name"].as_str().unwrap_or(""), k["days"], left, dev));
                if k["addr"].is_null() && ui.small_button("QR").clicked() {
                    self.qr_key = Some(id.clone());
                }
                if ui.small_button("продлить").on_hover_text("ещё на столько же дней от сегодня").clicked() {
                    self.access_op(json!({"cmd": "access", "op": "renew", "id": id, "days": k["days"]}), "Ключ продлён".into());
                }
                if !k["addr"].is_null() && ui.small_button("отвязать").on_hover_text("тем же ключом сможет войти новое устройство").clicked() {
                    self.access_op(json!({"cmd": "access", "op": "unbind", "id": id}), "Ключ отвязан".into());
                }
                if ui.small_button("удалить").clicked() {
                    self.access_op(json!({"cmd": "access", "op": "revoke", "id": id}), "Ключ удалён, доступ закрыт".into());
                }
            });
        }
        let reqs = acc["requests"].as_array().cloned().unwrap_or_default();
        if !reqs.is_empty() {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(format!("Запросы доступа ({}) — одобрить на {} дн.:", reqs.len(), self.new_days)).strong());
            for q in &reqs {
                let a = q["addr"].as_str().unwrap_or("").to_string();
                ui.horizontal_wrapped(|ui| {
                    ui.monospace(format!("{} · {} · {} назад", q["name"].as_str().unwrap_or(""), a, dur(now.saturating_sub(q["last"].as_u64().unwrap_or(0)))));
                    if let Some(m) = q["message"].as_str().filter(|m| !m.is_empty()) {
                        ui.label(format!("«{m}»"));
                    }
                    if ui.small_button("одобрить").clicked() {
                        self.access_op(json!({"cmd": "access", "op": "approve", "addr": a, "days": self.new_days}), "Доступ одобрен".into());
                    }
                    if ui.small_button("отклонить").clicked() {
                        self.access_op(json!({"cmd": "access", "op": "deny", "addr": a}), "Запрос отклонён".into());
                    }
                });
            }
        }
        if let Some(mut accepted) = self.public_dialog {
            let mut open = true;
            let mut done = false;
            egui::Window::new("Сделать сервер публичным?").open(&mut open).collapsible(false).resizable(false).show(ui.ctx(), |ui| {
                ui.label(acc["risk"].as_str().unwrap_or(""));
                ui.add_space(6.0);
                ui.checkbox(&mut accepted, "Я понимаю последствия и принимаю риск");
                ui.horizontal(|ui| {
                    if ui.add_enabled(accepted, egui::Button::new("Сделать публичным")).clicked() {
                        self.access_op(json!({"cmd": "access", "op": "public", "on": true, "accept_risk": true}), "Сервер стал публичным".into());
                        done = true;
                    }
                    if ui.button("Отмена").clicked() {
                        done = true;
                    }
                });
            });
            self.public_dialog = if open && !done { Some(accepted) } else { None };
        }
    }

    fn network_tab(&mut self, ui: &mut egui::Ui, st: Option<&Value>) {
        ui.heading("Трекеры");
        ui.label("Трекеры обходят сеть Yggdrasil и собирают работающие серверы выхода; служба спрашивает их каждые 2 мин, новые трекеры добавляются сами.");
        if let Some(c) = self.cfg.as_mut() {
            ui.checkbox(&mut c.network.use_trackers, "Получать серверы и пиры от трекеров");
        }
        ui.label("Трекеры, по одному в строке:");
        ui.add(egui::TextEdit::multiline(&mut self.trackers_text).desired_rows(3).desired_width(f32::INFINITY).code_editor());
        if let Some(t) = st.and_then(|s| s["trackers"]["status"].as_str()) {
            ui.label(format!("Последний опрос: {t}"));
        }
        if ui.button("Спросить трекеры сейчас").clicked() {
            let data = self.data.clone();
            std::thread::spawn(move || {
                let t = api::request(json!({"cmd": "refresh_trackers"})).map(|_| "Спрашиваю трекеры…".to_string()).unwrap_or_else(|e| e);
                data.lock().unwrap().toast = Some((t, Instant::now()));
            });
        }
        ui.separator();
        ui.heading("Сеть Yggdrasil");
        let Some(c) = self.cfg.as_mut() else { return };
        let n = &mut c.network;
        ui.checkbox(&mut n.public_peers, "Публичный список пиров (publicpeers.neilalexander.dev)");
        ui.label("Свои пиры, по одному в строке (tls://host:port, tcp://…, ?key=…):");
        ui.add(egui::TextEdit::multiline(&mut self.peers_text).desired_rows(4).desired_width(f32::INFINITY).code_editor());
        egui::Grid::new("net").num_columns(2).show(ui, |ui| {
            ui.label("Сколько пиров держать (1–32)");
            ui.add(egui::DragValue::new(&mut n.target).range(1..=32));
            ui.end_row();
            ui.label("Менять пиров, пока пинг до сервера выше, мс");
            ui.add(egui::DragValue::new(&mut n.rtt_limit).range(10..=5000));
            ui.end_row();
            ui.label("Пауза между заменами пиров, с");
            ui.add(egui::DragValue::new(&mut n.swap_interval).range(5..=3600));
            ui.end_row();
            ui.label("Сколько проверять нового пира до решения, с");
            ui.add(egui::DragValue::new(&mut n.swap_settle).range(10..=3600));
            ui.end_row();
            ui.label("Слотов для LAN-пиров (0 = как выше)");
            ui.add(egui::DragValue::new(&mut n.lan_target).range(0..=32));
            ui.end_row();
        });
        ui.checkbox(&mut n.lan_discovery, "Искать пиры в локальной сети (yggdrasil-go, TLS)");
        ui.label("Доп. аргументы tcp_node (для отладки):");
        ui.text_edit_singleline(&mut n.extra_args);
        ui.label("Узел пингует сервер каждые 10 с. Пока пинг выше порога, он заменяет самого медленного пира и проверяет нового заданное время; если пинг не упал хотя бы на 10 %, следующая замена ждёт вдвое дольше (до 10 мин).");
        ui.horizontal(|ui| {
            if ui.button("Сохранить и применить").clicked() {
                if let Some(c) = self.cfg.as_mut() {
                    c.network.peers = self.peers_text.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
                    c.network.trackers = self.trackers_text.lines().map(|l| l.trim().to_string()).filter(|l| config::parse_server(l).is_some()).collect();
                    self.trackers_loaded = c.network.trackers.clone();
                }
                self.apply("Сохранено");
            }
            if ui.button("Обновить публичный список").clicked() {
                let data = self.data.clone();
                std::thread::spawn(move || {
                    let t = api::request(json!({"cmd": "refresh_peers"})).map(|_| "Список обновляется".to_string()).unwrap_or_else(|e| e);
                    data.lock().unwrap().toast = Some((t, Instant::now()));
                });
            }
        });
    }

    fn status_tab(&mut self, ui: &mut egui::Ui, st: Option<&Value>) {
        ui.heading("Статус");
        let Some(s) = st else { return };
        let n = &s["node"];
        let mut t = format!("Узел Yggdrasil: {}\n", if n["running"].as_bool() == Some(true) { "работает" } else { "не запущен" });
        if let Some(stats) = n["stats"].as_object() {
            let nn = &stats["node"];
            let (key, root) = (nn["public_key"].as_str().unwrap_or(""), nn["root"].as_str().unwrap_or(""));
            let up = crate::api::peer_count(s);
            t += &format!(
                "Сеть: {}\nАдрес:  {}\nКлюч:   {}\nКорень: {}…\nАптайм: {}\nПиров: {} · сессий {} · путей {}\n",
                if up == 0 { "нет пиров ✖" } else if root == key { "только свой корень ⚠" } else { "подключено ✔" },
                nn["address"].as_str().unwrap_or(""),
                key,
                root.get(..16).unwrap_or(root),
                dur(stats.get("uptime_s").and_then(|v| v.as_u64()).unwrap_or(0)),
                up,
                nn["sessions"],
                nn["paths"]
            );
        }
        if let Some(l) = n["lan"].as_str() {
            t += &format!("Поиск в LAN: {l}\n");
        }
        let tp = crate::api::tracker_pings(s);
        if !tp.is_empty() {
            t += "\nТрекеры (пинг узла каждые 10 с):\n";
            for (a, l, avg) in tp {
                t += &format!("  {a}  {}\n", crate::api::ping_text(l, avg));
            }
        }
        let c = &s["client"];
        if c["enabled"].as_bool() == Some(true) {
            t += &format!(
                "\nVPN: {} → {}\nПинг до сервера: {}\n",
                if c["active"].as_bool() == Some(true) { "работает" } else { "запускается" },
                c["server"].as_str().unwrap_or(""),
                c["server_rtt"].as_str().unwrap_or("измеряю…")
            );
            if let Some(sw) = c["last_swap"].as_str() {
                t += &format!("Замена пира: {sw}\n");
            }
        }
        let sv = &s["server"];
        if sv["enabled"].as_bool() == Some(true) {
            let l = &sv["limits"];
            t += &format!(
                "\nСервер выхода: {} · клиентов {}\nОграничения: сервер ↓ {} ↑ {} · клиент ↓ {} ↑ {}\n",
                if sv["active"].as_bool() == Some(true) { "работает" } else { "запускается" },
                sv["clients"].as_array().map(|a| a.len()).unwrap_or(0),
                limit_text(l["total_down"].as_u64().unwrap_or(0)),
                limit_text(l["total_up"].as_u64().unwrap_or(0)),
                limit_text(l["client_down"].as_u64().unwrap_or(0)),
                limit_text(l["client_up"].as_u64().unwrap_or(0))
            );
        }
        if let Some(e) = s["last_error"].as_str() {
            t += &format!("\nПоследняя ошибка: {e}\n");
        }
        ui.monospace(t);
        ui.separator();
        ui.heading("Пиры");
        ui.label("Нажмите на пира — подробности. ★ — через него сейчас идёт больше всего трафика.");
        let peers = &n["peers"];
        ui.monospace(format!("Всего сейчас: ↓ {}  ↑ {}", mbit(peers["rx_bps"].as_u64().unwrap_or(0)), mbit(peers["tx_bps"].as_u64().unwrap_or(0))));
        let list = peers["peers"].as_array().cloned().unwrap_or_default();
        let busiest = list.iter().map(|p| p["rx_bps"].as_u64().unwrap_or(0) + p["tx_bps"].as_u64().unwrap_or(0)).max().unwrap_or(0);
        for p in list {
            let rate = p["rx_bps"].as_u64().unwrap_or(0) + p["tx_bps"].as_u64().unwrap_or(0);
            let host = p["uri"].as_str().filter(|u| !u.is_empty()).map(|u| u.split("://").nth(1).unwrap_or(u).split('?').next().unwrap_or(u).to_string())
                .unwrap_or_else(|| format!("входящий {}", p["address"].as_str().unwrap_or("")));
            let text = format!(
                "{} {host}  {}\n    ↓ {}  ↑ {}  · {} %",
                if rate == busiest && rate > 500 { "★" } else { " " },
                p["rtt_ms"].as_u64().map(|r| format!("{r} мс")).unwrap_or_default(),
                mbit(p["rx_bps"].as_u64().unwrap_or(0)),
                mbit(p["tx_bps"].as_u64().unwrap_or(0)),
                p["share"]
            );
            if ui.add(egui::Button::new(egui::RichText::new(text).monospace()).frame(false)).clicked() {
                self.peer_detail = Some(p.clone());
            }
        }
    }

    fn logs_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Копировать").clicked() {
                ui.ctx().copy_text(self.data.lock().unwrap().logs.join("\n"));
                self.toast("Скопировано");
            }
            if ui.button("Очистить").clicked() {
                self.data.lock().unwrap().logs.clear();
                let _ = api::request(json!({"cmd": "clear_logs"}));
            }
        });
        let logs = self.data.lock().unwrap().logs.join("\n");
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            ui.add(egui::Label::new(egui::RichText::new(logs).monospace().size(11.5)).wrap());
        });
    }
}

impl App {
    fn shot_step(&mut self, ctx: &egui::Context) {
        const TABS: [(Tab, &str); 5] = [(Tab::Client, "client"), (Tab::Server, "server"), (Tab::Network, "network"), (Tab::Status, "status"), (Tab::Logs, "logs")];
        let Some((dir, i, at)) = self.shot.clone() else { return };
        let got = ctx.input(|inp| {
            inp.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = got {
            let [w, h] = img.size;
            let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
            for p in &img.pixels {
                out.extend_from_slice(&[p.r(), p.g(), p.b()]);
            }
            let _ = std::fs::write(format!("{dir}/{}.ppm", TABS[i].1), out);
            if i + 1 == TABS.len() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            self.shot = Some((dir, i + 1, Instant::now()));
        } else if i < TABS.len() && at.elapsed() > Duration::from_secs(if i == 0 { 4 } else { 1 }) {
            self.tab = TABS[i].0;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            self.shot = Some((dir, i, Instant::now() + Duration::from_secs(3600)));
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.shot_step(ctx);
        let (status, error, toast) = {
            let d = self.data.lock().unwrap();
            (d.status.clone(), d.error.clone(), d.toast.clone())
        };
        if self.cfg.is_none() && status.is_some() {
            self.reload_config();
        }
        if let Some(list) = status.as_ref().and_then(|s| s["trackers"]["list"].as_array()) {
            let list: Vec<String> = list.iter().filter_map(|v| v.as_str().map(String::from)).collect();
            if list != self.trackers_loaded && self.trackers_text == self.trackers_loaded.join("\n") {
                if let Some(c) = self.cfg.as_mut() {
                    c.network.trackers = list.clone();
                }
                self.trackers_text = list.join("\n");
                self.trackers_loaded = list;
            }
        }
        egui::TopBottomPanel::bottom("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                for (t, name) in [(Tab::Client, "Клиент"), (Tab::Server, "Сервер"), (Tab::Network, "Сеть"), (Tab::Status, "Статус"), (Tab::Logs, "Логи")] {
                    if ui.selectable_label(self.tab == t, egui::RichText::new(name).size(16.0)).clicked() {
                        self.tab = t;
                    }
                }
            });
        });
        if let Some(u) = status.as_ref().map(|s| s["update"].clone()).filter(|u| u.is_object()) {
            egui::TopBottomPanel::top("update").show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(format!("Доступна версия {} (у вас {})", u["version"].as_str().unwrap_or("?"), crate::update::CURRENT)).strong());
                    if let Some(url) = u["url"].as_str()
                        && ui.button("Скачать").clicked() {
                            crate::update::open_url(url);
                        }
                    if ui.button("Что нового").clicked() {
                        crate::update::open_url(u["page"].as_str().unwrap_or(crate::update::PAGE));
                    }
                    #[cfg(unix)]
                    if ui.button("Обновить (pkexec)").clicked()
                        && let Ok(me) = std::env::current_exe() {
                            let _ = std::process::Command::new("pkexec").arg(me).arg("update").spawn();
                        }
                });
            });
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(e) = &error {
                ui.colored_label(egui::Color32::from_rgb(200, 60, 40), e);
                #[cfg(unix)]
                if ui.button("Установить и запустить службу (pkexec)").clicked()
                    && let Ok(me) = std::env::current_exe() {
                        let _ = std::process::Command::new("pkexec").arg(me).arg("install").spawn();
                    }
                #[cfg(windows)]
                if ui.button("Установить и запустить службу (нужны права администратора)").clicked() {
                    crate::install::install_elevated_detached();
                }
                ui.separator();
            }
            if self.tab == Tab::Logs {
                self.logs_tab(ui);
                return;
            }
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.tab {
                Tab::Client => self.client_tab(ui, status.as_ref()),
                Tab::Server => self.server_tab(ui, status.as_ref()),
                Tab::Network => self.network_tab(ui, status.as_ref()),
                Tab::Status => self.status_tab(ui, status.as_ref()),
                Tab::Logs => {}
            });
        });
        if let Some(p) = self.peer_detail.clone() {
            let mut open = true;
            egui::Window::new("Пир").open(&mut open).collapsible(false).show(ctx, |ui| {
                ui.monospace(format!(
                    "Адрес пира: {}\nНаправление: {}{}\nПинг: {}\nСессия: {}\n\nСейчас: ↓ {}  ↑ {}\nДоля трафика узла: {} %\nЗа сессию: ↓ {}  ↑ {}\n\nYggdrasil-адрес: {}\nКлюч: {}",
                    p["uri"].as_str().filter(|u| !u.is_empty()).unwrap_or("входящее соединение"),
                    if p["dir"].as_str() == Some("out") { "исходящий" } else { "входящий" },
                    if p["lan"].as_bool() == Some(true) { " · найден в LAN" } else { "" },
                    p["rtt_ms"].as_u64().map(|r| format!("{r} мс")).unwrap_or("—".into()),
                    dur(p["up_s"].as_u64().unwrap_or(0)),
                    mbit(p["rx_bps"].as_u64().unwrap_or(0)),
                    mbit(p["tx_bps"].as_u64().unwrap_or(0)),
                    p["share"],
                    human(p["rx"].as_u64().unwrap_or(0)),
                    human(p["tx"].as_u64().unwrap_or(0)),
                    p["address"].as_str().unwrap_or(""),
                    p["key"].as_str().unwrap_or("")
                ));
                if ui.button("Копировать адрес пира").clicked() {
                    ui.ctx().copy_text(p["uri"].as_str().filter(|u| !u.is_empty()).or(p["address"].as_str()).unwrap_or("").to_string());
                }
            });
            if !open {
                self.peer_detail = None;
            }
        }
        if let Some((t, at)) = toast
            && at.elapsed() < Duration::from_secs(3) {
                egui::Area::new(egui::Id::new("toast")).anchor(egui::Align2::CENTER_BOTTOM, [0.0, -60.0]).show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| ui.label(t));
                });
                ctx.request_repaint_after(Duration::from_millis(500));
            }
    }
}

/// How the window is drawn. Linux: OpenGL (glow). Windows: wgpu — DirectX 12, which also works
/// in virtual machines without 3D acceleration (WARP), then Vulkan and OpenGL ES.
/// `YGGPROXY_RENDERER=glow|wgpu` overrides.
fn renderer() -> eframe::Renderer {
    match std::env::var("YGGPROXY_RENDERER").as_deref() {
        #[cfg(windows)]
        Ok("glow") => eframe::Renderer::Glow,
        #[cfg(windows)]
        _ => eframe::Renderer::Wgpu,
        #[cfg(not(windows))]
        _ => eframe::Renderer::Glow,
    }
}

pub fn run() -> Result<(), String> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("Ratatoskr").with_inner_size([560.0, 760.0]),
        renderer: renderer(),
        ..Default::default()
    };
    eframe::run_native("Ratatoskr", opts, Box::new(|cc| Ok(Box::new(App::new(cc))))).map_err(|e| format!("окно: {e}"))
}
