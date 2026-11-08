mod catalog;
#[cfg(unix)]
pub(crate) mod credentials;
mod details;
pub mod discovery;
mod health;
pub mod http;
mod imports;
pub mod model;
mod native;
mod plex;
mod requests;
pub mod service;
pub mod store;
mod supervisor;
pub mod upstream;

#[cfg(test)]
mod tests;
