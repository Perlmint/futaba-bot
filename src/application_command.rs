#![allow(dead_code)]
use serenity::model::interactions::application_command;

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

impl PartialEq<application_command::ApplicationCommandOptionType> for ApplicationCommandOptionType {
    fn eq(&self, other: &application_command::ApplicationCommandOptionType) -> bool {
        *self as u8 == *other as u8
    }
}

#[derive(Debug, serde::Serialize)]
pub struct ApplicationCommandOptionChoice<'a> {
    pub name: &'a str,
    pub value: serde_json::Value,
}

impl<'a> PartialEq<application_command::ApplicationCommandOptionChoice>
    for ApplicationCommandOptionChoice<'a>
{
    fn eq(&self, other: &application_command::ApplicationCommandOptionChoice) -> bool {
        self.name == other.name && self.value == other.name
    }
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
}

impl<'a> PartialEq<application_command::ApplicationCommandOption> for ApplicationCommandOption<'a> {
    fn eq(&self, other: &application_command::ApplicationCommandOption) -> bool {
        PartialEq::eq(&self.kind, &other.kind)
            && self.name == other.name
            && self.description == other.description
            && self.required.unwrap_or(false) == other.required
            && PartialEq::eq(&self.options, &other.options)
            && PartialEq::eq(&self.choices, &other.choices)
    }
}

#[derive(Debug, serde::Serialize)]
pub struct ApplicationCommand<'a> {
    pub name: &'a str,
    pub description: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ApplicationCommandOption<'a>>,
}

impl<'a> PartialEq<application_command::ApplicationCommand> for ApplicationCommand<'a> {
    fn eq(&self, other: &application_command::ApplicationCommand) -> bool {
        self.name == other.name
            && self.description == other.description
            && PartialEq::eq(&self.options, &other.options)
    }
}
