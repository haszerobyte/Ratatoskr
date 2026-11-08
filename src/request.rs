//! What a search asks for, beyond the words: how many results, where, how
//! recent, how safe, which page. Every engine reads what it supports and
//! ignores the rest (an encyclopedia has no "past week").

use std::fmt;
use std::str::FromStr;

/// How strictly adult content is filtered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SafeSearch {
    On,
    #[default]
    Moderate,
    Off,
}

/// How recent results must be.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimeLimit {
    Day,
    Week,
    Month,
    Year,
}

impl TimeLimit {
    /// The one-letter code most engines use: d, w, m, y.
    pub fn letter(self) -> &'static str {
        match self {
            Self::Day => "d",
            Self::Week => "w",
            Self::Month => "m",
            Self::Year => "y",
        }
    }
}

/// An option value that is not one of the allowed ones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BadOption {
    pub option: &'static str,
    pub value: String,
    pub allowed: &'static str,
}

impl fmt::Display for BadOption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} must be one of {}, not {:?}", self.option, self.allowed, self.value)
    }
}

impl std::error::Error for BadOption {}

impl FromStr for SafeSearch {
    type Err = BadOption;
    fn from_str(value: &str) -> Result<Self, BadOption> {
        match value.trim().to_ascii_lowercase().as_str() {
            "on" | "strict" => Ok(Self::On),
            "moderate" => Ok(Self::Moderate),
            "off" => Ok(Self::Off),
            _ => Err(BadOption { option: "safesearch", value: value.into(), allowed: "on, moderate, off" }),
        }
    }
}

impl FromStr for TimeLimit {
    type Err = BadOption;
    fn from_str(value: &str) -> Result<Self, BadOption> {
        match value.trim().to_ascii_lowercase().as_str() {
            "d" | "day" => Ok(Self::Day),
            "w" | "week" => Ok(Self::Week),
            "m" | "month" => Ok(Self::Month),
            "y" | "year" => Ok(Self::Year),
            _ => Err(BadOption { option: "time", value: value.into(), allowed: "day, week, month, year" }),
        }
    }
}

/// A search: the words and how to search for them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchRequest {
    pub query: String,
    /// At most this many results.
    pub limit: usize,
    /// Localize to a country: an ISO 3166-1 alpha-2 code ("US", "IN", "DE").
    pub country: Option<String>,
    pub safesearch: SafeSearch,
    pub time: Option<TimeLimit>,
    /// The page of results, from 1.
    pub page: u32,
}

impl SearchRequest {
    pub fn new(query: impl Into<String>, limit: usize) -> Self {
        Self { query: query.into(), limit, country: None, safesearch: SafeSearch::default(), time: None, page: 1 }
    }

    pub fn country(mut self, country: Option<&str>) -> Self {
        self.country = country.map(str::trim).filter(|c| !c.is_empty()).map(str::to_ascii_uppercase);
        self
    }

    pub fn safesearch(mut self, safesearch: SafeSearch) -> Self {
        self.safesearch = safesearch;
        self
    }

    pub fn time(mut self, time: Option<TimeLimit>) -> Self {
        self.time = time;
        self
    }

    pub fn page(mut self, page: u32) -> Self {
        self.page = page.max(1);
        self
    }

    /// The same request for `limit` results.
    pub fn with_limit(&self, limit: usize) -> Self {
        Self { limit, ..self.clone() }
    }

    /// Whether only the first page, at any date, is asked for: the plain
    /// search an encyclopedia's one best page belongs to.
    pub fn is_plain(&self) -> bool {
        self.page == 1 && self.time.is_none()
    }

    /// The key a cached answer to this request is stored under.
    pub fn cache_key(&self, category: &str) -> String {
        format!(
            "{category}\u{0}{}\u{0}{}\u{0}{:?}\u{0}{:?}\u{0}{}\u{0}{}",
            self.limit,
            self.country.as_deref().unwrap_or(""),
            self.safesearch,
            self.time,
            self.page,
            self.query.trim().to_lowercase()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_parse_and_normalize() {
        assert_eq!("Week".parse::<TimeLimit>().unwrap(), TimeLimit::Week);
        assert_eq!("d".parse::<TimeLimit>().unwrap().letter(), "d");
        assert_eq!("strict".parse::<SafeSearch>().unwrap(), SafeSearch::On);
        let error = "fortnight".parse::<TimeLimit>().unwrap_err();
        assert_eq!(error.to_string(), "time must be one of day, week, month, year, not \"fortnight\"");
        let request = SearchRequest::new("Rust", 5).country(Some(" in ")).page(0).time(Some(TimeLimit::Day));
        assert_eq!((request.country.as_deref(), request.page), (Some("IN"), 1));
        assert!(!request.is_plain() && SearchRequest::new("x", 5).is_plain());
        assert_ne!(
            request.cache_key("text"),
            request.clone().time(None).cache_key("text"),
            "options make their own cache entry"
        );
        assert_ne!(request.cache_key("text"), request.cache_key("news"));
    }
}
