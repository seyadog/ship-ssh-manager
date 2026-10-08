# Arquitectura de ship

TUI en Rust (ratatui + crossterm) para gestionar conexiones SSH. Soporta ratón y corre en Linux, macOS y Windows.

## Idea central

Cada pestaña lanza el `ssh` del sistema dentro de un PTY (`portable-pty`). Su salida pasa por un emulador de terminal (`vt100`) y se dibuja en un widget de ratatui. Así `ship` hereda gratis:

- agente SSH, claves, `~/.ssh/config`, ProxyJump
- `known_hosts`: ssh pregunta por host keys nuevas o cambiadas **dentro de la pestaña** y las guarda él mismo
- mensajes de error reales del cliente (host inalcanzable, permiso denegado, etc.)

`ship` añade encima: el árbol de servidores, las pestañas, los formularios y un resumen claro cuando una sesión termina.

## Módulos

| Módulo | Responsabilidad |
|---|---|
| `main.rs` | Terminal (raw mode, ratón, paste), bucle de eventos, CLI |
| `store.rs` | Modelo (carpetas, servidores) y persistencia JSON en `~/.config/ship/servers.json` |
| `session.rs` | PTY + emulador: spawn, resize, escritura, scrollback, detección de salida |
| `keys.rs` | Traduce teclas de crossterm a bytes de terminal |
| `app.rs` | Estado de la app, foco, modales, manejo de teclado y ratón |
| `ui.rs` | Dibujo: sidebar, pestañas, terminal, formularios |

Todo vive en un solo proceso. No hay IPC ni renderer separado.

## Seguridad

- Ninguna contraseña se escribe en disco. Con autenticación por contraseña, `ssh` la pide dentro del PTY.
- Fase posterior: guardado opcional en el keyring del sistema (crate `keyring`), nunca en el JSON.
- Se prefieren claves y agente. El archivo de datos solo guarda la *ruta* de la clave.

## Fases

1. **MVP:** árbol, formulario, pestañas, resize, errores claros, drag & drop.
2. **CLI y búsqueda:** `ship <alias>`, `ship list`, `ship add`, importar `~/.ssh/config`, Ctrl+K, reconexión automática.
3. **Extras:** SFTP, túneles guardados, snippets, temas.
