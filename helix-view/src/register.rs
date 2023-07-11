use std::{borrow::Cow, collections::HashMap, iter};

use anyhow::Result;
use helix_core::NATIVE_LINE_ENDING;

use crate::{
    clipboard::{get_clipboard_provider, ClipboardProvider, ClipboardType},
    document::SCRATCH_BUFFER_NAME,
    Editor,
};

/// A key-value store for saving sets of values.
///
/// Each register corresponds to a `char`. Most chars can be used to store any set of
/// values but a few chars are "special registers". Special registers have unique
/// behaviors when read or written to:
///
/// * Black hole (`_`): all values read and written are discarded
/// * Selection indices (`#`): index number of each selection starting at 1
/// * Selection contents (`.`)
/// * Document path (`%`): filename of the current buffer
/// * System clipboard (`*`)
/// * Primary clipboard (`+`)
#[derive(Debug)]
pub struct Registers {
    /// The mapping of register to values.
    /// Values are stored in reverse order when inserted with `Registers::write`.
    /// The order is reversed again in `Registers::read`. This allows us to
    /// efficiently prepend new values in `Registers::push`.
    inner: HashMap<char, Vec<String>>,
    clipboard_provider: Box<dyn ClipboardProvider>,
}

impl Default for Registers {
    fn default() -> Self {
        Self {
            inner: Default::default(),
            clipboard_provider: get_clipboard_provider(),
        }
    }
}

// Some special registers must allocate their values while others and regular
// registers can hand out borrowed values.
type RegisterValues<'a> = Box<dyn ExactSizeIterator<Item = Cow<'a, str>> + 'a>;

impl Registers {
    pub fn read<'a>(&'a self, name: char, editor: &'a Editor) -> Option<RegisterValues<'a>> {
        match name {
            '_' => Some(Box::new(iter::empty())),
            '#' => {
                let (view, doc) = current_ref!(editor);
                let selections = doc.selection(view.id).len();
                // ExactSizeIterator is implemented for Range<usize> but
                // not RangeInclusive<usize>.
                Some(Box::new(
                    (0..selections).map(|i| (i + 1).to_string().into()),
                ))
            }
            '.' => {
                let (view, doc) = current_ref!(editor);
                let text = doc.text().slice(..);
                Some(Box::new(doc.selection(view.id).fragments(text)))
            }
            '%' => {
                let doc = doc!(editor);

                let path = doc
                    .path()
                    .as_ref()
                    .map(|p| p.to_string_lossy())
                    .unwrap_or_else(|| SCRATCH_BUFFER_NAME.into());

                Some(Box::new(iter::once(path)))
            }
            '*' | '+' => Some(read_from_clipboard(
                self.clipboard_provider.as_ref(),
                self.inner.get(&name),
                match name {
                    '*' => ClipboardType::Clipboard,
                    '+' => ClipboardType::Selection,
                    _ => unreachable!(),
                },
            )),
            _ => self
                .inner
                .get(&name)
                .map(|values| Box::new(values.iter().map(Cow::from).rev()) as RegisterValues),
        }
    }

    pub fn write(&mut self, name: char, mut values: Vec<String>) -> Result<()> {
        match name {
            '_' => Ok(()),
            '#' | '.' | '%' => Err(anyhow::anyhow!("Register {name} does not support writing")),
            '*' | '+' => {
                self.clipboard_provider.set_contents(
                    values.join(NATIVE_LINE_ENDING.as_str()),
                    match name {
                        '*' => ClipboardType::Clipboard,
                        '+' => ClipboardType::Selection,
                        _ => unreachable!(),
                    },
                )?;
                values.reverse();
                self.inner.insert(name, values);
                Ok(())
            }
            _ => {
                values.reverse();
                self.inner.insert(name, values);
                Ok(())
            }
        }
    }

    pub fn push(&mut self, name: char, value: String) -> Result<()> {
        match name {
            '_' => Ok(()),
            '#' | '.' | '%' => Err(anyhow::anyhow!("Register {name} does not support pushing")),
            '*' | '+' => {
                let clipboard_type = match name {
                    '*' => ClipboardType::Clipboard,
                    '+' => ClipboardType::Selection,
                    _ => unreachable!(),
                };

                let mut values: Vec<_> = read_from_clipboard(
                    self.clipboard_provider.as_ref(),
                    self.inner.get(&name),
                    clipboard_type,
                )
                .map(|value| value.to_string())
                .collect();
                values.reverse();
                values.push(value);

                self.clipboard_provider
                    .set_contents(values.join(NATIVE_LINE_ENDING.as_str()), clipboard_type)?;
                values.reverse();
                self.inner.insert(name, values);

                Ok(())
            }
            _ => {
                self.inner.entry(name).or_insert_with(Vec::new).push(value);
                Ok(())
            }
        }
    }

    pub fn first<'a>(&'a self, name: char, editor: &'a Editor) -> Option<Cow<'a, str>> {
        self.read(name, editor).and_then(|mut values| values.next())
    }

    pub fn last<'a>(&'a self, name: char, editor: &'a Editor) -> Option<Cow<'a, str>> {
        self.read(name, editor).and_then(|values| values.last())
    }

    pub fn iter_preview(&self) -> impl Iterator<Item = (char, &str)> {
        self.inner
            .iter()
            .filter(|(name, _)| !matches!(name, '*' | '+'))
            .map(|(name, values)| {
                let preview = values
                    .last()
                    .and_then(|s| s.lines().next())
                    .unwrap_or("<empty>");

                (*name, preview)
            })
            .chain(
                [
                    ('_', "<empty>"),
                    ('#', "<selection indices>"),
                    ('.', "<selection contents>"),
                    ('%', "<document path>"),
                    ('*', "<system clipboard>"),
                    ('+', "<primary clipboard>"),
                ]
                .iter()
                .copied(),
            )
    }

    pub fn clear(&mut self) {
        self.inner.clear()
    }

    pub fn remove(&mut self, name: char) -> bool {
        match name {
            '_' | '#' | '.' | '%' | '*' | '+' => false,
            _ => self.inner.remove(&name).is_some(),
        }
    }
}

fn read_from_clipboard<'a>(
    provider: &dyn ClipboardProvider,
    saved_values: Option<&'a Vec<String>>,
    clipboard_type: ClipboardType,
) -> RegisterValues<'a> {
    match provider.get_contents(clipboard_type) {
        Ok(contents) => {
            // If we're pasting the same values that we just yanked, re-use
            // the saved values. This allows pasting multiple selections
            // even when yanked to a clipboard.
            let Some(values) = saved_values else { return Box::new(iter::once(contents.into())) };

            if contents_are_saved(values, &contents) {
                Box::new(values.iter().map(Cow::from).rev())
            } else {
                Box::new(iter::once(contents.into()))
            }
        }
        Err(err) => {
            log::error!(
                "Failed to read {} clipboard: {err}",
                match clipboard_type {
                    ClipboardType::Clipboard => "system",
                    ClipboardType::Selection => "primary",
                }
            );

            Box::new(iter::empty())
        }
    }
}

fn contents_are_saved(saved_values: &[String], mut contents: &str) -> bool {
    let line_ending = NATIVE_LINE_ENDING.as_str();
    let mut values = saved_values.iter().rev();

    match values.next() {
        Some(first) if contents.starts_with(first) => {
            contents = &contents[first.len()..];
        }
        _ => return false,
    }

    for value in values {
        if contents.starts_with(line_ending) && contents[line_ending.len()..].starts_with(value) {
            contents = &contents[line_ending.len() + value.len()..];
        } else {
            return false;
        }
    }

    true
}
