//! The catalog tree: catalogs, schemas, tables and their columns, each level
//! loaded the first time it is expanded.
use quill_core::metadata::{CatalogRef, ColumnMetadata, SchemaRef, TableRef};

use crate::task::catalog::{Children, Load};

pub enum Item {
    Catalog(CatalogRef),
    Schema(SchemaRef),
    Table(TableRef),
    Column(ColumnMetadata),
}

impl Item {
    fn label(&self) -> String {
        match self {
            Self::Catalog(c) => c.name.clone(),
            Self::Schema(s) => s.name.clone(),
            Self::Table(t) => t.name.clone(),
            Self::Column(c) => format!("{} {}", c.name, c.native_type),
        }
    }

    /// What expanding this item loads; `None` for leaves.
    fn load(&self) -> Option<Load> {
        Some(match self {
            Self::Catalog(c) => Load::Schemas(c.clone()),
            Self::Schema(s) => Load::Tables(s.clone()),
            Self::Table(t) => Load::Columns(t.clone()),
            Self::Column(_) => return None,
        })
    }
}

pub enum Kids {
    Unloaded,
    Loading,
    Loaded(Vec<Node>),
    Failed(String),
}

impl Kids {
    fn from(children: Children) -> Self {
        let items: Vec<Item> = match children {
            Children::Catalogs(v) => v.into_iter().map(Item::Catalog).collect(),
            Children::Schemas(v) => v.into_iter().map(Item::Schema).collect(),
            Children::Tables(v) => v.into_iter().map(Item::Table).collect(),
            Children::Columns(v) => v.into_iter().map(Item::Column).collect(),
        };
        Self::Loaded(items.into_iter().map(Node::new).collect())
    }
}

pub struct Node {
    pub item: Item,
    pub expanded: bool,
    pub kids: Kids,
}

impl Node {
    const fn new(item: Item) -> Self {
        Self {
            item,
            expanded: false,
            kids: Kids::Unloaded,
        }
    }
}

/// One visible line of the tree.
pub enum Line {
    Node {
        path: Vec<usize>,
        depth: usize,
        label: String,
        expanded: bool,
        leaf: bool,
    },
    /// Loading or failure beneath an expanded node.
    Status {
        depth: usize,
        text: String,
        failed: bool,
    },
}

pub struct Tree {
    roots: Kids,
}

impl Tree {
    pub const fn new() -> Self {
        Self {
            roots: Kids::Unloaded,
        }
    }

    /// Mark the roots loading if they have not been requested yet.
    pub fn begin_roots(&mut self) -> bool {
        let unloaded = matches!(self.roots, Kids::Unloaded);
        if unloaded {
            self.roots = Kids::Loading;
        }
        unloaded
    }

    pub fn node(&self, path: &[usize]) -> Option<&Node> {
        let (first, rest) = path.split_first()?;
        let Kids::Loaded(nodes) = &self.roots else {
            return None;
        };
        rest.iter()
            .try_fold(nodes.get(*first)?, |node, i| match &node.kids {
                Kids::Loaded(kids) => kids.get(*i),
                _ => None,
            })
    }

    fn node_mut(&mut self, path: &[usize]) -> Option<&mut Node> {
        let (first, rest) = path.split_first()?;
        let Kids::Loaded(nodes) = &mut self.roots else {
            return None;
        };
        rest.iter()
            .try_fold(nodes.get_mut(*first)?, |node, i| match &mut node.kids {
                Kids::Loaded(kids) => kids.get_mut(*i),
                _ => None,
            })
    }

    /// Expand `path`, returning what to load if its children are not loaded.
    pub fn expand(&mut self, path: &[usize]) -> Option<Load> {
        let node = self.node_mut(path)?;
        let load = node.item.load()?;
        node.expanded = true;
        match node.kids {
            Kids::Unloaded | Kids::Failed(_) => {
                node.kids = Kids::Loading;
                Some(load)
            }
            Kids::Loading | Kids::Loaded(_) => None,
        }
    }

    pub fn collapse(&mut self, path: &[usize]) -> bool {
        match self.node_mut(path) {
            Some(node) if node.expanded => {
                node.expanded = false;
                true
            }
            _ => false,
        }
    }

    /// Store loaded children under `path` (the roots when empty). Returns
    /// what to load next when the roots are a single catalog, which is
    /// expanded automatically.
    pub fn loaded(
        &mut self,
        path: &[usize],
        result: quill_core::error::Result<Children>,
    ) -> Option<Load> {
        let kids = match result {
            Ok(children) => Kids::from(children),
            Err(e) => Kids::Failed(crate::connection::error_chain(&e)),
        };
        if path.is_empty() {
            self.roots = kids;
            return match &self.roots {
                Kids::Loaded(nodes) if nodes.len() == 1 => self.expand(&[0]),
                _ => None,
            };
        }
        if let Some(node) = self.node_mut(path) {
            node.kids = kids;
        }
        None
    }

    pub fn lines(&self) -> Vec<Line> {
        let mut lines = Vec::new();
        Self::push_kids(&self.roots, &mut Vec::new(), 0, &mut lines);
        lines
    }

    fn push_kids(kids: &Kids, path: &mut Vec<usize>, depth: usize, lines: &mut Vec<Line>) {
        match kids {
            Kids::Unloaded => {}
            Kids::Loading => lines.push(Line::Status {
                depth,
                text: "loading…".into(),
                failed: false,
            }),
            Kids::Failed(error) => lines.push(Line::Status {
                depth,
                text: error.clone(),
                failed: true,
            }),
            Kids::Loaded(nodes) => {
                for (i, node) in nodes.iter().enumerate() {
                    path.push(i);
                    lines.push(Line::Node {
                        path: path.clone(),
                        depth,
                        label: node.item.label(),
                        expanded: node.expanded,
                        leaf: node.item.load().is_none(),
                    });
                    if node.expanded {
                        Self::push_kids(&node.kids, path, depth + 1, lines);
                    }
                    path.pop();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog(name: &str) -> CatalogRef {
        CatalogRef { name: name.into() }
    }

    fn schema(name: &str) -> SchemaRef {
        SchemaRef {
            catalog: catalog("db"),
            name: name.into(),
        }
    }

    fn labels(tree: &Tree) -> Vec<String> {
        tree.lines()
            .into_iter()
            .map(|line| match line {
                Line::Node { depth, label, .. } => format!("{}{label}", "  ".repeat(depth)),
                Line::Status { depth, text, .. } => format!("{}({text})", "  ".repeat(depth)),
            })
            .collect()
    }

    #[test]
    fn a_single_catalog_expands_itself_and_levels_load_lazily() {
        let mut tree = Tree::new();
        assert!(tree.begin_roots());
        assert!(!tree.begin_roots());
        let next = tree.loaded(&[], Ok(Children::Catalogs(vec![catalog("db")])));
        assert!(matches!(next, Some(Load::Schemas(_))));
        assert_eq!(labels(&tree), ["db", "  (loading…)"]);

        tree.loaded(
            &[0],
            Ok(Children::Schemas(vec![schema("public"), schema("audit")])),
        );
        assert!(matches!(tree.expand(&[0, 1]), Some(Load::Tables(_))));
        tree.loaded(
            &[0, 1],
            Err(quill_core::error::Error::UnsupportedOperation(
                "list tables",
            )),
        );
        assert_eq!(
            labels(&tree),
            [
                "db",
                "  public",
                "  audit",
                "    (operation is not supported: list tables)"
            ]
        );

        assert!(tree.collapse(&[0, 1]));
        assert_eq!(labels(&tree), ["db", "  public", "  audit"]);
        // Failed levels retry on the next expand.
        assert!(tree.expand(&[0, 1]).is_some());
    }
}
