use std::{borrow::Cow, collections::HashMap, iter};

use anyhow::Result;

use crate::Editor;

/// A key-value store for saving sets of values.
///
/// Each register corresponds to a `char`. Most chars can be used to store any set of
/// values but a few chars are "special registers". Special registers have unique
/// behaviors when read or written to:
///
/// * Black hole (`_`): all values read and written are discarded
/// * Selection indices (`#`): index number of each selection starting at 1
/// * Selection contents (`.`)
#[derive(Debug, Default)]
pub struct Registers {
    inner: HashMap<char, Vec<String>>,
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
            _ => self
                .inner
                .get(&name)
                .map(|values| Box::new(values.iter().map(Cow::from)) as RegisterValues),
        }
    }

    pub fn write(&mut self, name: char, values: Vec<String>) -> Result<()> {
        match name {
            '_' => Ok(()),
            '#' | '.' => Err(anyhow::anyhow!("Register {name} does not support writing")),
            _ => {
                self.inner.insert(name, values);
                Ok(())
            }
        }
    }

    pub fn push(&mut self, name: char, value: String) -> Result<()> {
        match name {
            '_' => Ok(()),
            '#' | '.' => Err(anyhow::anyhow!("Register {name} does not support pushing")),
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
            .map(|(name, values)| {
                let preview = values
                    .first()
                    .and_then(|s| s.lines().next())
                    .unwrap_or("<empty>");

                (*name, preview)
            })
            .chain(
                [
                    ('_', "<empty>"),
                    ('#', "<selection indices>"),
                    ('.', "<selection contents>"),
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
            '_' | '#' | '.' => false,
            _ => self.inner.remove(&name).is_some(),
        }
    }
}
