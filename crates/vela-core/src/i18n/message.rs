//! Message descriptors: a stable id and its English source text, plus the values bound
//! to its arguments.

use std::borrow::Cow;

/// A value bound to a message argument.
#[derive(Clone, Debug, PartialEq)]
pub enum MessageArg {
    Text(Cow<'static, str>),
    Number(f64),
    /// A message, translated first (`with_values(MOVE_TO, [("pane", msg(...).into())])`).
    Message(MessageDescriptor),
}

impl From<f64> for MessageArg {
    fn from(v: f64) -> Self {
        MessageArg::Number(v)
    }
}

impl From<usize> for MessageArg {
    fn from(v: usize) -> Self {
        MessageArg::Number(v as f64)
    }
}

impl From<&'static str> for MessageArg {
    fn from(v: &'static str) -> Self {
        MessageArg::Text(Cow::Borrowed(v))
    }
}

impl From<String> for MessageArg {
    fn from(v: String) -> Self {
        MessageArg::Text(Cow::Owned(v))
    }
}

impl From<MessageDescriptor> for MessageArg {
    fn from(v: MessageDescriptor) -> Self {
        MessageArg::Message(v)
    }
}

/// A translatable message: a stable id and its English text (ICU message syntax).
#[derive(Clone, Debug, PartialEq)]
pub struct MessageDescriptor {
    pub id: Cow<'static, str>,
    pub default_message: Cow<'static, str>,
    /// Values bound to the message's arguments (see [`with_values`]); values passed to a
    /// translator's `t_with` take precedence.
    pub values: Vec<(Cow<'static, str>, MessageArg)>,
}

/// Display text: plain text renders verbatim, a descriptor is translated.
#[derive(Clone, Debug, PartialEq)]
pub enum MessageLike {
    Text(Cow<'static, str>),
    Message(MessageDescriptor),
}

impl Default for MessageLike {
    fn default() -> Self {
        MessageLike::Text(Cow::Borrowed(""))
    }
}

impl From<&'static str> for MessageLike {
    fn from(v: &'static str) -> Self {
        MessageLike::Text(Cow::Borrowed(v))
    }
}

impl From<String> for MessageLike {
    fn from(v: String) -> Self {
        MessageLike::Text(Cow::Owned(v))
    }
}

impl From<MessageDescriptor> for MessageLike {
    fn from(v: MessageDescriptor) -> Self {
        MessageLike::Message(v)
    }
}

impl MessageLike {
    /// Whether this is a translatable descriptor rather than plain text.
    pub fn is_message(&self) -> bool {
        matches!(self, MessageLike::Message(_))
    }

    /// The English source text (see [`source_text`](super::source_text)).
    pub fn source_text(&self) -> String {
        super::source_text(self)
    }
}

/// Declare a translatable message: a stable dotted id and its English text.
pub fn msg(id: &'static str, default_message: &'static str) -> MessageLike {
    MessageLike::Message(MessageDescriptor {
        id: Cow::Borrowed(id),
        default_message: Cow::Borrowed(default_message),
        values: Vec::new(),
    })
}

/// A message with values bound to its arguments (`with_values(&BAND, [("n", 2.into())])`
/// reads "Band #2"). Later bindings of the same name win. Plain text is returned as is.
pub fn with_values<I>(message: &MessageLike, values: I) -> MessageLike
where
    I: IntoIterator<Item = (&'static str, MessageArg)>,
{
    match message {
        MessageLike::Text(_) => message.clone(),
        MessageLike::Message(d) => {
            let mut out = d.clone();
            for (name, value) in values {
                bind(&mut out.values, Cow::Borrowed(name), value);
            }
            MessageLike::Message(out)
        }
    }
}

/// Bind `value` to `name`, replacing an earlier binding of the same name.
pub(crate) fn bind(
    values: &mut Vec<(Cow<'static, str>, MessageArg)>,
    name: Cow<'static, str>,
    value: MessageArg,
) {
    match values.iter_mut().find(|(n, _)| *n == name) {
        Some(slot) => slot.1 = value,
        None => values.push((name, value)),
    }
}

/// The translatable form of display text: the descriptor, or nothing for plain text.
pub fn message_only(m: Option<&MessageLike>) -> Option<&MessageLike> {
    m.filter(|m| m.is_message())
}
