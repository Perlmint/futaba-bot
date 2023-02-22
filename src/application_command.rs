#![allow(dead_code)]

#[derive(Debug, Clone, Copy, serde_repr::Serialize_repr)]
#[repr(u8)]
pub enum ApplicationCommandOptionType {
    SubCommand = 1,
    SubCommandGroup = 2,
    String = 3,
    Integer = 4,
    Boolean = 5,
    User = 6,
    Channel = 7,
    Role = 8,
    Mentionable = 9,
    Number = 10,
}
#[derive(Debug, serde::Serialize)]
pub struct ApplicationCommandOptionChoice<'a> {
    pub name: &'a str,
    pub value: serde_json::Value,
}

#[derive(Debug, serde::Serialize)]
pub struct ApplicationCommandOption<'a> {
    #[serde(rename = "type")]
    pub kind: ApplicationCommandOptionType,
    pub name: &'a str,
    pub description: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<ApplicationCommandOptionChoice<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ApplicationCommandOption<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autocomplete: Option<bool>
}

#[derive(Debug, serde::Serialize)]
pub struct ApplicationCommand<'a> {
    pub name: &'a str,
    pub description: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ApplicationCommandOption<'a>>,
}
