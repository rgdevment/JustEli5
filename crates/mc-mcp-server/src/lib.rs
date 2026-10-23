//! `mc mcp`: a Model Context Protocol server over stdio that lets an AI client read and
//! act on the meetings in the corpus folder.

mod meetings;
mod resources;
pub mod rpc;
mod schema;
mod search;
mod series;
mod server;
mod tools;
mod transcript;

use std::io::{self, BufRead, Write};
use std::path::Path;

pub use series::SeriesIndex;
pub use server::{PROTOCOL_VERSION, Server};

/// Serves `input` until EOF. Only protocol messages go to `out`.
pub fn serve(
    corpus_dir: &Path,
    series: SeriesIndex,
    input: impl BufRead,
    out: &mut dyn Write,
) -> io::Result<()> {
    rpc::serve(input, out, &mut Server::new(corpus_dir, series))
}
