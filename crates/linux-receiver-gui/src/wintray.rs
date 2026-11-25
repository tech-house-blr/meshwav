//! The Windows notification-area icon: hover text, left click shows the window, menu has Show and Quit.
use super::TrayCmd;
use std::sync::mpsc::Sender;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

pub struct WinTray {
    icon: TrayIcon,
}

impl WinTray {
    pub fn new(rgba: Vec<u8>, w: u32, h: u32, tx: Sender<TrayCmd>, ctx: eframe::egui::Context) -> Result<Self, String> {
        let menu = Menu::new();
        let show = MenuItem::new("Show window", true, None);
        let quit = MenuItem::new("Quit", true, None);
        menu.append_items(&[&show, &quit]).map_err(|e| e.to_string())?;
        let (show_id, quit_id) = (show.id().clone(), quit.id().clone());
        let (tx2, ctx2) = (tx.clone(), ctx.clone());
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            if e.id == show_id {
                let _ = tx.send(TrayCmd::Show);
            } else if e.id == quit_id {
                let _ = tx.send(TrayCmd::Quit);
            }
            ctx.request_repaint();
        }));
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                let _ = tx2.send(TrayCmd::Show);
                ctx2.request_repaint();
            }
        }));
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip("Meshwav")
            .with_icon(Icon::from_rgba(rgba, w, h).map_err(|e| e.to_string())?)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { icon })
    }

    pub fn set_state(&self, state: &str) {
        let _ = self.icon.set_tooltip(Some(format!("Meshwav: {state}")));
    }
}
