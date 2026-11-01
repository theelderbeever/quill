//! Commands typed into the `:` palette.
use crate::{
    action::Action,
    connection::{ConnectionId, Connections, Target},
    view::Route,
};

#[derive(Debug, PartialEq)]
pub enum Command {
    /// `:conn` lists connections; `:conn <name>` opens one.
    Conn(Option<Target>),
    /// `:disconnect [name]` closes the named or current connection.
    Disconnect(Option<ConnectionId>),
    /// `:save <path>` exports the shown result; the extension picks the format.
    Save(std::path::PathBuf),
    Quit,
}

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum CommandError {
    #[error("no connection is open here; name one")]
    NoConnection,
    #[error("invalid save path: {0}")]
    SavePath(String),
    #[error("type a command")]
    Empty,
    #[error("unknown command `{0}`")]
    Unknown(String),
    #[error("no connection named `{0}`")]
    UnknownConnection(String),
    #[error("several connections are named `{0}`; use x in the connection list")]
    AmbiguousConnection(String),
    #[error("`{0}` needs an argument")]
    MissingArgument(String),
    #[error("too many arguments for `{0}`")]
    TooManyArguments(String),
}

impl Command {
    /// Parse palette input, resolving names against configured connections.
    pub fn parse(input: &str, connections: &Connections) -> Result<Self, CommandError> {
        let lookup = |name: &str| {
            connections
                .lookup(name)
                .ok_or_else(|| CommandError::UnknownConnection(name.to_owned()))
        };
        let mut words = input.split_whitespace();
        let name = words.next().ok_or(CommandError::Empty)?;
        let command = match name {
            "conn" | "connection" => Self::Conn(words.next().map(lookup).transpose()?),
            "disconnect" | "close" => Self::Disconnect(
                words
                    .next()
                    .map(|n| match lookup(n)? {
                        Target::One(id) => Ok(id),
                        Target::Many(_) => Err(CommandError::AmbiguousConnection(n.to_owned())),
                    })
                    .transpose()?,
            ),
            // The rest of the line, so paths may contain spaces.
            "save" | "w" => {
                let path = input.trim_start()[name.len()..].trim();
                if path.is_empty() {
                    return Err(CommandError::MissingArgument(name.to_owned()));
                }
                return Ok(Self::Save(
                    crate::path::SavePath::expand(path).map_err(CommandError::SavePath)?,
                ));
            }
            "q" | "quit" => Self::Quit,
            other => return Err(CommandError::Unknown(other.to_owned())),
        };
        if words.next().is_some() {
            return Err(CommandError::TooManyArguments(name.to_owned()));
        }
        Ok(command)
    }

    /// `current` is the connection of the view the command was typed in.
    pub fn into_action(self, current: Option<&ConnectionId>) -> Result<Action, CommandError> {
        Ok(match self {
            Self::Conn(target) => Action::Jump(Route::target(target)),
            Self::Disconnect(id) => Action::Disconnect(
                id.or_else(|| current.cloned())
                    .ok_or(CommandError::NoConnection)?,
            ),
            Self::Save(path) => Action::Save(path),
            Self::Quit => Action::Quit,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        connection::testing::{connections, group, id},
        filter::Filter,
    };

    #[test]
    fn parses_commands_and_aliases() {
        let connections = connections(&["pg"]);
        let pg = id(&connections, "pg");
        let parse = |input| Command::parse(input, &connections);
        assert_eq!(parse("conn"), Ok(Command::Conn(None)));
        assert_eq!(
            parse("  connection pg "),
            Ok(Command::Conn(Some(Target::One(pg.clone()))))
        );
        assert_eq!(parse("q"), Ok(Command::Quit));
        assert_eq!(parse("quit"), Ok(Command::Quit));
        assert_eq!(parse("close"), Ok(Command::Disconnect(None)));
        assert_eq!(parse("disconnect pg"), Ok(Command::Disconnect(Some(pg))));
        assert_eq!(
            parse("save out/my result.csv"),
            Ok(Command::Save("out/my result.csv".into()))
        );
        assert_eq!(
            parse("save"),
            Err(CommandError::MissingArgument("save".into()))
        );
    }

    #[test]
    fn ambiguous_names_filter_or_refuse() {
        let connections = Connections::new([group("home", &["pg"]), group("project", &["pg"])]);
        let parse = |input| Command::parse(input, &connections);
        assert_eq!(
            parse("conn pg"),
            Ok(Command::Conn(Some(Target::Many(Filter::exact("pg")))))
        );
        assert_eq!(
            parse("disconnect pg"),
            Err(CommandError::AmbiguousConnection("pg".into()))
        );
    }

    #[test]
    fn rejects_bad_input() {
        let connections = connections(&["pg"]);
        let parse = |input| Command::parse(input, &connections);
        assert_eq!(parse(" "), Err(CommandError::Empty));
        assert_eq!(parse("nope"), Err(CommandError::Unknown("nope".into())));
        assert_eq!(
            parse("conn other"),
            Err(CommandError::UnknownConnection("other".into()))
        );
        assert_eq!(
            parse("q now"),
            Err(CommandError::TooManyArguments("q".into()))
        );
    }
}
