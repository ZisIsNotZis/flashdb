# Active work

- [07-persistent-tiles](07-persistent-tiles/issues/07-persistent-tiles.md) — claimed from `77537a0`; Designer checkpoint recorded; unsafe first tile commit `d1bbe44` remains unintegrated (review BLOCK). Fresh isolated worker `8797c901-e3ce-4b18-8223-209b7acf51d4` redesigns safe owned-I/O tile read-through from clean master `9fb162a`. Parent next: inspect/review its commit, integrate only if no SIGBUS/panic path, run gates and fault injection. WAL retained; no manifest/rotation. No user decision pending.
