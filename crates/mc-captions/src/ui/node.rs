//! The tree model. Fields mirror what UI Automation exposes; the JSON names are
//! short (`aid`, `class`, `rid`) because fixtures are written by hand and by
//! `mc doctor --dump`.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    Window,
    Pane,
    Document,
    Group,
    Text,
    List,
    ListItem,
    Edit,
    Button,
    Other(String),
}

impl Role {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Window => "Window",
            Self::Pane => "Pane",
            Self::Document => "Document",
            Self::Group => "Group",
            Self::Text => "Text",
            Self::List => "List",
            Self::ListItem => "ListItem",
            Self::Edit => "Edit",
            Self::Button => "Button",
            Self::Other(name) => name,
        }
    }

    pub fn parse(name: &str) -> Self {
        match name {
            "Window" => Self::Window,
            "Pane" => Self::Pane,
            "Document" => Self::Document,
            "Group" => Self::Group,
            "Text" => Self::Text,
            "List" => Self::List,
            "ListItem" => Self::ListItem,
            "Edit" => Self::Edit,
            "Button" => Self::Button,
            other => Self::Other(other.to_string()),
        }
    }

    /// From a Windows UIA `ControlTypeName` such as `GroupControl`.
    pub fn from_uia(control_type_name: &str) -> Self {
        // "Control" alone has nothing left to strip.
        match control_type_name.strip_suffix("Control") {
            Some(rest) if !rest.is_empty() => Self::parse(rest),
            _ => Self::parse(control_type_name),
        }
    }
}

impl Serialize for Role {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Role {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        if name.is_empty() {
            return Err(D::Error::custom("empty role"));
        }
        Ok(Self::parse(&name))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiNode {
    pub role: Role,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "aid")]
    pub automation_id: String,
    #[serde(default, rename = "class")]
    pub class_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, rename = "rid", skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<Vec<i32>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<UiNode>,
}

impl UiNode {
    pub fn new(role: Role, name: &str) -> Self {
        Self {
            role,
            name: name.to_string(),
            automation_id: String::new(),
            class_name: String::new(),
            value: None,
            runtime_id: None,
            children: Vec::new(),
        }
    }

    pub fn with_children(mut self, children: Vec<UiNode>) -> Self {
        self.children = children;
        self
    }

    pub fn with_aid(mut self, aid: &str) -> Self {
        self.automation_id = aid.to_string();
        self
    }

    /// Number of nodes in this subtree (itself included).
    pub fn count(&self) -> usize {
        1 + self.children.iter().map(Self::count).sum::<usize>()
    }

    /// The node at a child-index path, if the path exists.
    pub fn at(&self, path: &[usize]) -> Option<&Self> {
        path.iter()
            .try_fold(self, |node, index| node.children.get(*index))
    }
}

/// A top-level window as listed by the backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    pub handle: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default, rename = "class")]
    pub class_name: String,
    #[serde(default)]
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
}
