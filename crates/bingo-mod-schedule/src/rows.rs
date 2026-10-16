use bingo_services::schedule::{self, Listing, Pending};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Selection {
    Schedule(String),
    Wake,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Row {
    Schedule(schedule::Row),
    Wake(Pending),
}

impl Row {
    pub fn selection(&self) -> Selection {
        match self {
            Self::Schedule(row) => Selection::Schedule(row.id.clone()),
            Self::Wake(_) => Selection::Wake,
        }
    }

    pub fn schedule(&self) -> Option<&schedule::Row> {
        match self {
            Self::Schedule(row) => Some(row),
            Self::Wake(_) => None,
        }
    }
}

pub(crate) fn rows(listing: &Listing, wake: Option<&Pending>) -> Vec<Row> {
    listing
        .entries
        .iter()
        .cloned()
        .map(Row::Schedule)
        .chain(wake.cloned().map(Row::Wake))
        .collect()
}

pub(crate) fn selected(rows: &[Row], selection: Option<&Selection>) -> usize {
    rows.iter()
        .position(|row| Some(&row.selection()) == selection)
        .unwrap_or(0)
}
