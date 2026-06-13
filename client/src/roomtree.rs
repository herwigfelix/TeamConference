//! Räume-und-Nutzer-Baum, plattformspezifisch mit dem jeweils **nativen**
//! (und damit barrierefreien) Widget umgesetzt:
//!   - Windows: `wxTreeCtrl` (native SysTreeView32, MSAA)
//!   - macOS/Linux: `wxDataViewTreeCtrl` (native NSOutlineView/GtkTreeView)
//!
//! Beide Implementierungen bieten dieselbe API: `build`, `rebuild`, `selected`,
//! `set_focus`. So bleibt der restliche Code plattformneutral.

use std::collections::{HashMap, HashSet};

use wxdragon::prelude::*;

use crate::protocol::{RoomInfo, UserInfo};

/// Was ein Baumknoten repräsentiert.
#[derive(Debug, Clone)]
pub enum NodeRef {
    Room(i64),
    User { id: i64, room: i64 },
}

fn user_label(u: &UserInfo) -> String {
    let mut label = u.nickname.clone();
    if !u.role.is_empty() && u.role != "user" {
        label.push_str(&format!(" [{}]", u.role));
    }
    if u.muted {
        label.push_str(", stumm");
    }
    if u.deafened {
        label.push_str(", taub");
    }
    label
}

fn room_label(room: &RoomInfo) -> String {
    let lock = if room.has_password { ", Passwort" } else { "" };
    format!("{} ({} Nutzer{})", room.name, room.users.len(), lock)
}

/// IDs des aktuellen Raums und aller Vorfahren (zum Aufklappen des Pfades).
fn expand_path(rooms: &[RoomInfo], current: Option<i64>) -> HashSet<i64> {
    let mut set = HashSet::new();
    let mut cur = current;
    while let Some(id) = cur {
        if !set.insert(id) {
            break;
        }
        cur = rooms.iter().find(|r| r.id == id).and_then(|r| r.parent_id);
    }
    set
}

// ───────────────────────── Windows: wxTreeCtrl ─────────────────────────
#[cfg(target_os = "windows")]
mod imp {
    use super::*;

    pub type Widget = TreeCtrl;

    pub fn build(parent: &Panel) -> Widget {
        TreeCtrl::builder(parent)
            .with_style(TreeCtrlStyle::HasButtons | TreeCtrlStyle::LinesAtRoot)
            .build()
    }

    fn add_level(
        tree: &Widget,
        parent_item: &TreeItemId,
        rooms: &[RoomInfo],
        parent: Option<i64>,
        expand: &HashSet<i64>,
    ) {
        let mut level: Vec<&RoomInfo> = rooms.iter().filter(|r| r.parent_id == parent).collect();
        level.sort_by(|a, b| a.name.cmp(&b.name));
        for room in level {
            let Some(item) = tree.append_item_with_data(
                parent_item,
                &room_label(room),
                NodeRef::Room(room.id),
                None,
                None,
            ) else {
                continue;
            };
            for u in &room.users {
                tree.append_item_with_data(
                    &item,
                    &user_label(u),
                    NodeRef::User { id: u.id, room: room.id },
                    None,
                    None,
                );
            }
            add_level(tree, &item, rooms, Some(room.id), expand);
            if expand.contains(&room.id) {
                tree.expand(&item);
            }
        }
    }

    // map wird auf Windows nicht gebraucht (Item-Data am TreeCtrl).
    pub fn rebuild(
        tree: &Widget,
        rooms: &[RoomInfo],
        current: Option<i64>,
        _map: &mut HashMap<usize, NodeRef>,
    ) {
        tree.delete_all_items();
        let expand = expand_path(rooms, current);
        if let Some(root) = tree.add_root("Räume", None, None) {
            add_level(tree, &root, rooms, None, &expand);
            tree.expand(&root);
        }
    }

    pub fn selected(tree: &Widget, _map: &HashMap<usize, NodeRef>) -> Option<NodeRef> {
        let item = tree.get_selection()?;
        let data = tree.get_custom_data(&item)?;
        data.downcast_ref::<NodeRef>().cloned()
    }
}

// ────────────────── macOS/Linux: wxDataViewTreeCtrl ──────────────────
#[cfg(not(target_os = "windows"))]
mod imp {
    use super::*;

    pub type Widget = DataViewTreeCtrl;

    pub fn build(parent: &Panel) -> Widget {
        let tree = DataViewTreeCtrl::builder(parent).build();
        tree.append_icon_text_column(
            "Räume und Nutzer",
            0,
            240,
            DataViewAlign::Left,
            DataViewColumnFlags::Resizable,
        );
        tree
    }

    fn item_key(item: &DataViewItem) -> Option<usize> {
        item.get_id::<u8>().map(|p| p as usize)
    }

    fn add_level(
        tree: &Widget,
        parent_item: &DataViewItem,
        rooms: &[RoomInfo],
        parent: Option<i64>,
        expand: &HashSet<i64>,
        map: &mut HashMap<usize, NodeRef>,
    ) {
        let mut level: Vec<&RoomInfo> = rooms.iter().filter(|r| r.parent_id == parent).collect();
        level.sort_by(|a, b| a.name.cmp(&b.name));
        for room in level {
            let item = tree.append_container(parent_item, &room_label(room), -1, -1);
            if let Some(k) = item_key(&item) {
                map.insert(k, NodeRef::Room(room.id));
            }
            for u in &room.users {
                let ui = tree.append_item(&item, &user_label(u), -1);
                if let Some(k) = item_key(&ui) {
                    map.insert(k, NodeRef::User { id: u.id, room: room.id });
                }
            }
            add_level(tree, &item, rooms, Some(room.id), expand, map);
            if expand.contains(&room.id) {
                tree.expand(&item);
            }
        }
    }

    pub fn rebuild(
        tree: &Widget,
        rooms: &[RoomInfo],
        current: Option<i64>,
        map: &mut HashMap<usize, NodeRef>,
    ) {
        tree.delete_all_items();
        map.clear();
        let expand = expand_path(rooms, current);
        let root = DataViewItem::default();
        add_level(tree, &root, rooms, None, &expand, map);
    }

    pub fn selected(tree: &Widget, map: &HashMap<usize, NodeRef>) -> Option<NodeRef> {
        let item = tree.get_selection()?;
        let key = item.get_id::<u8>().map(|p| p as usize)?;
        map.get(&key).cloned()
    }
}

pub use imp::{build, rebuild, selected, Widget};

/// Ausgewählte Raum-ID (bei einem Nutzer-Knoten dessen Raum).
pub fn selected_room(tree: &Widget, map: &HashMap<usize, NodeRef>) -> Option<i64> {
    match selected(tree, map)? {
        NodeRef::Room(id) => Some(id),
        NodeRef::User { room, .. } => Some(room),
    }
}

/// Ausgewählte Nutzer-ID (nur wenn ein Nutzer-Knoten gewählt ist).
pub fn selected_user(tree: &Widget, map: &HashMap<usize, NodeRef>) -> Option<i64> {
    match selected(tree, map)? {
        NodeRef::User { id, .. } => Some(id),
        NodeRef::Room(_) => None,
    }
}
