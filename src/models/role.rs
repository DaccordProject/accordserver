use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Role {
    pub id: String,
    pub name: String,
    pub color: i64,
    pub hoist: bool,
    pub icon: Option<String>,
    pub position: i64,
    pub permissions: Vec<String>,
    pub managed: bool,
    pub mentionable: bool,
}

#[derive(Debug, Clone)]
pub struct RoleRow {
    pub id: String,
    pub space_id: String,
    pub name: String,
    pub color: i64,
    pub hoist: bool,
    pub icon: Option<String>,
    pub position: i64,
    pub permissions: String, // JSON array string
    pub managed: bool,
    pub mentionable: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateRole {
    pub name: String,
    pub color: Option<i64>,
    pub hoist: Option<bool>,
    pub permissions: Option<Vec<String>>,
    pub mentionable: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRole {
    pub name: Option<String>,
    pub color: Option<i64>,
    pub hoist: Option<bool>,
    pub icon: Option<String>,
    pub position: Option<i64>,
    pub permissions: Option<Vec<String>>,
    pub mentionable: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct RolePositionUpdate {
    pub id: String,
    pub position: i64,
}
