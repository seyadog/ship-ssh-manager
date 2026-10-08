# ship

Gestor de conexiones SSH para la terminal: una TUI moderna, con ratón. Guarda tus servidores en carpetas y abre cada sesión en una pestaña. Alternativa ligera a Tabby.

> Estado: Fase 1 (MVP) en desarrollo. Arquitectura en [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Desarrollo

Requiere [Rust](https://rustup.rs) estable y un cliente `ssh` en el PATH.

```sh
cargo run            # ejecutar
cargo test           # pruebas
cargo build --release
```

Los datos se guardan en `~/.config/ship/servers.json` (o en `$SHIP_CONFIG_DIR`). Las contraseñas y passphrases van al keyring del sistema, nunca a ese archivo.

## Uso

**Ratón:** doble clic abre un servidor o despliega una carpeta; arrastra para mover (sobre una carpeta, dentro de ella; sobre un servidor, justo antes de él); arrastra pestañas para reordenarlas; clic en ✕ cierra una pestaña; doble clic en una pestaña la renombra; la rueda recorre el historial del terminal.

**Panel lateral:**

| Tecla | Acción |
|---|---|
| `↑` `↓` / `j` `k` | mover selección |
| `Enter` | abrir servidor / plegar carpeta |
| `a` / `f` | nuevo servidor / nueva carpeta |
| `e` | editar |
| `d` | borrar |
| `Alt+↑` `Alt+↓` | cambiar orden entre hermanos |
| `q` | salir |

**Global:** `F6` panel ⇄ terminal · `Alt+←/→` cambiar de pestaña · `Alt+Shift+←/→` mover pestaña · `Alt+1..9` ir a pestaña · `Alt+W` cerrar · `F2` renombrar · `Shift+PgUp/PgDn` historial.

En el formulario, `Ctrl+O` (o «Examinar…») abre un selector de archivos que empieza en `~/.ssh` y resalta las claves privadas.

## Seguridad

- La sesión es el `ssh` del sistema dentro de un PTY: `known_hosts`, agente y claves funcionan como siempre. Si la host key es nueva o cambió, `ssh` pregunta dentro de la pestaña.
- Si guardas una contraseña o passphrase, se almacena en el keyring del sistema y se envía una sola vez cuando `ssh` la pide.

## Plan

1. **MVP** (en curso): árbol, formulario, pestañas, resize, errores, arrastrar y soltar.
2. CLI `ship <alias>` / `list` / `add`, importar `~/.ssh/config`, Ctrl+K, reconexión automática.
3. SFTP, túneles, snippets, temas.
