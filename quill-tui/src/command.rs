//! Commands typed into the `:` palette.
use crate::{
    action::Action,
    connection::{ConnectionName, Connections},
    view::Route,
};

#[derive(Debug, PartialEq)]
pub enum Command {
    /// `:conn` lists connections; `:conn <name>` opens one.
    Conn(Option<ConnectionName>),
    /// `:disconnect [name]` closes the named or current connection.
    Disconnect(Option<ConnectionName>),
    Quit,
}

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum CommandError {
    #[error("no connection is open here; name one")]
    NoConnection,
    #[error("type a command")]
    Empty,
    #[error("unknown command `{0}`")]
    Unknown(String),
    #[error("no connection named `{0}`")]
    UnknownConnection(String),
    #[error("too many arguments for `{0}`")]
    TooManyArguments(String),
}

impl Command {
    /// Parse palette input, resolving names against configured connections.
    pub fn parse(input: &str, connections: &Connections) -> Result<Self, CommandError> {
        let resolve = |word: Option<&str>| {
            word.map(|n| {
                connections
                    .resolve(n)
                    .cloned()
                    .ok_or_else(|| CommandError::UnknownConnection(n.to_owned()))
            })
            .transpose()
        };
        let mut words = input.split_whitespace();
        let name = words.next().ok_or(CommandError::Empty)?;
        let command = match name {
            "conn" | "connection" => Self::Conn(resolve(words.next())?),
            "disconnect" | "close" => Self::Disconnect(resolve(words.next())?),
            "q" | "quit" => Self::Quit,
            other => return Err(CommandError::Unknown(other.to_owned())),
        };
        if words.next().is_some() {
            return Err(CommandError::TooManyArguments(name.to_owned()));
        }
        Ok(command)
    }
}

impl Command {
    /// `current` is the connection of the view the command was typed in.
    pub fn into_action(self, current: Option<&ConnectionName>) -> Result<Action, CommandError> {
        Ok(match self {
            Self::Conn(None) => Action::Jump(Route::Connections),
            Self::Conn(Some(name)) => Action::Jump(Route::Queries(name)),
            Self::Disconnect(name) => Action::Disconnect(
                name.or_else(|| current.cloned())
                    .ok_or(CommandError::NoConnection)?,
            ),
            Self::Quit => Action::Quit,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::testing::connections;

    #[test]
    fn parses_commands_and_aliases() {
        let connections = connections(&["pg"]);
        let pg = connections.resolve("pg").cloned();
        let parse = |input| Command::parse(input, &connections);
        assert_eq!(parse("conn"), Ok(Command::Conn(None)));
        assert_eq!(parse("  connection pg "), Ok(Command::Conn(pg)));
        assert_eq!(parse("q"), Ok(Command::Quit));
        assert_eq!(parse("quit"), Ok(Command::Quit));
        assert_eq!(parse("close"), Ok(Command::Disconnect(None)));
        assert_eq!(
            parse("disconnect pg"),
            Ok(Command::Disconnect(connections.resolve("pg").cloned()))
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
