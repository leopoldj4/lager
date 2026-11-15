//! Prints the TypeScript types of the web interface's operations;
//! `scripts/wire.ts` writes them to `web/src/lib/wire.ts`
//! (docs/decisions/0035-generated-wire-types.md).

fn main() {
    print!("{}", lager_server::web::ops::typescript());
}
