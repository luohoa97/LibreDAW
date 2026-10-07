// SPDX-License-Identifier: GPL-3.0-or-later
//! Cached render-node layers and pango layouts for the custom widgets
//! (docs/ui-design.md 4.5). A widget keeps one `LayerCache` per static layer
//! and re-appends the node every frame; the layer is rebuilt only when its
//! key changes (data, size, scroll, zoom, or style).

use std::collections::HashMap;
use std::time::Instant;

use gtk::prelude::*;
use gtk::{gsk, pango};

use crate::perf;

/// One cached layer. `K` is whatever decides whether the layer is still
/// valid.
pub struct LayerCache<K: PartialEq> {
    key: Option<K>,
    node: Option<gsk::RenderNode>,
    name: &'static str,
}

impl<K: PartialEq> LayerCache<K> {
    pub fn new(name: &'static str) -> LayerCache<K> {
        LayerCache {
            key: None,
            node: None,
            name,
        }
    }

    /// Forgets the layer (the next `append` rebuilds it).
    pub fn invalidate(&mut self) {
        self.key = None;
        self.node = None;
    }

    pub fn is_valid_for(&self, key: &K) -> bool {
        self.key.as_ref() == Some(key)
    }

    /// Appends the layer to `s`, rebuilding it with `build` first if `key`
    /// differs from the one it was built with.
    pub fn append(&mut self, s: &gtk::Snapshot, key: K, build: impl FnOnce(&gtk::Snapshot)) {
        if !self.is_valid_for(&key) {
            let t = Instant::now();
            let rec = gtk::Snapshot::new();
            build(&rec);
            self.node = rec.to_node();
            self.key = Some(key);
            perf::rebuilt(self.name, t.elapsed());
        }
        if let Some(n) = &self.node {
            s.append_node(n);
        }
    }
}

/// Pango layouts by (text, bold, size), so repeated labels are shaped once.
/// Clear it when fonts or the scale change.
#[derive(Default)]
pub struct LayoutCache {
    map: HashMap<(String, bool), pango::Layout>,
}

impl LayoutCache {
    pub fn clear(&mut self) {
        self.map.clear();
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The layout of `text` in the widget's font.
    pub fn get(&mut self, w: &impl IsA<gtk::Widget>, text: &str, bold: bool) -> pango::Layout {
        // Bounded: a project with thousands of distinct labels must not grow
        // the cache without limit.
        if self.map.len() > 512 {
            self.map.clear();
        }
        self.map
            .entry((text.to_string(), bold))
            .or_insert_with(|| {
                let l = w.create_pango_layout(Some(text));
                if bold {
                    let attrs = pango::AttrList::new();
                    attrs.insert(pango::AttrInt::new_weight(pango::Weight::Bold));
                    l.set_attributes(Some(&attrs));
                }
                l
            })
            .clone()
    }
}
