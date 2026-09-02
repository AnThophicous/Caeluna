# Wayland/X11 compatibility boundary

`src/linux/compat.rs` is the compatibility boundary for the native Rouch
session. It is deliberately independent from the Liquid Glass shell: it does
not render, create windows, allocate GPU resources, or import a D-Bus/UI
crate. The shell can keep its Tahoe-like presentation while clients use the
native Wayland path first and a carefully bounded X11 fallback when it is
actually available.

## Truthful capability states

Every entry is represented by a `CapabilityState` and `ProbeEvidence`:

| State | Meaning | Permitted behaviour |
| --- | --- | --- |
| `Available` | The owner of the protocol or D-Bus connection confirmed the operation. | Use the real transport. |
| `Degraded` | A prerequisite was found, but the operation or bridge was not confirmed. | Use native Wayland or a bounded fallback; explain the limitation. |
| `Unavailable` | A required prerequisite is missing or a confirmed probe failed. | Disable the operation and keep the session usable. |

Finding an executable, an environment variable, or a D-Bus `.service` file is
only `Detected`, which maps to `Degraded`. This is intentional: an activatable
service is not proof that a session-bus name is owned, and an XWayland binary
is not proof that the compositor has installed the X11 bridge.

`CapabilityMatrix::detect()` performs the cheap local snapshot once. It does
not poll from the render loop. A real Wayland/D-Bus owner can construct a
`WaylandProbe`/`PortalProbe` with `Confirmed` observations and rebuild the
matrix after an explicit session or service change.

## XWayland adapter

`XWaylandAdapter::from_environment` looks for an absolute override in
`ROUCH_XWAYLAND`, then searches non-empty `PATH` entries for `Xwayland`. Empty
`PATH` entries are skipped so the current working directory is never searched
implicitly. It also requires a valid `WAYLAND_DISPLAY` and an existing
absolute `XDG_RUNTIME_DIR` before it reports that a child can be started.

The process boundary is fixed and uses `std::process::Command` directly:

```text
Xwayland -rootless -terminate -displayfd 1
```

The adapter overrides only these inherited environment values:

```text
WAYLAND_DISPLAY=<current Wayland socket>
XDG_RUNTIME_DIR=<current runtime directory>
XDG_SESSION_TYPE=wayland
```

There is no `sh -c`, `bash -c`, free-form argv, or free-form shell command.
The display number is drained from the child stdout pipe and exposed as
`:N`. `XWaylandProcess` reports `Running`/`Exited`, supports a bounded
`wait_for_display`, and kills/reaps the child on explicit shutdown or `Drop`.
The reader is one blocked thread per running XWayland child; it does not spin
or render.

`start_or_fallback()` returns either `Running(process)` or an explicit
`Fallback { reason }`. The fallback is native Wayland; it must not set
`DISPLAY` or route an X11 application to a non-existent server.

### What this adapter does not claim

Launching XWayland is only one half of compatibility. A native compositor must
also create the X11 socket/bridge, bind the XWayland Wayland protocol, forward
seats/outputs, and export `DISPLAY=:{N}` to X11 clients. Until the compositor
reports `WaylandProbe::xwayland_bridge = Confirmed`, the matrix reports X11
applications as `Degraded` even when the process is launchable.

The file is therefore a safe process/lifecycle adapter, not a fake X11
implementation. Wiring the bridge belongs to the native session owner and is
outside this agent's two-file boundary.

## Clipboard, data-device, and drag-and-drop contracts

The real core protocol names are exposed under `wayland_data_device`:

- `wl_data_device_manager`
- `wl_data_device`
- `wl_data_source`
- `wl_data_offer`
- optional `zwp_primary_selection_v1`

`DataOfferContract` validates MIME names, removes duplicates, and caps an offer
at 64 MIME types with a 255-byte MIME-name limit. `DataDeviceRequest` carries
only typed operations (`ReadClipboard`, `WriteClipboard`, `AcceptDrop`,
`FinishDrop`, `Cancel`); it does not claim that bytes were transferred.

`DataDeviceTransport` and `DragAndDropTransport` are the narrow contracts for
the Smithay/Wayland owner to implement. They must map to real
`wl_data_device`/`wl_data_offer` requests and return an error when the
corresponding `DataDeviceContract` is not `Available`. Clipboard and DnD are
not implemented through a portal by default: the ordinary Wayland path is the
core data-device protocol. Portal clipboard is a separate, session-scoped
feature and does not replace it.

## Portal and D-Bus capability matrix

This module has no D-Bus dependency, so local detection is deliberately
conservative. `PortalProbe::from_environment` checks only
`DBUS_SESSION_BUS_ADDRESS` and bounded standard D-Bus service-file locations.
It never invokes `dbus-send`, `busctl`, a shell, or a portal request. A real
D-Bus adapter should set `Confirmed` only after it connects to the session bus,
checks the interface/version, and handles the actual response/error.

| Capability | Real owner/API | `Available` requires | Degraded fallback | Unavailable limit |
| --- | --- | --- | --- | --- |
| Session D-Bus | Session bus | A live connection is confirmed | Show “bus address found; not connected” | Do not call portals or external notifications |
| Portals | `org.freedesktop.portal.Desktop` at `/org/freedesktop/portal/desktop` | Bus name/interface is confirmed | Keep native file/open flows; retry only on explicit refresh | Do not pretend a Flatpak permission exists |
| Portal clipboard | `org.freedesktop.portal.Clipboard` | Compatible session plus `RequestClipboard`/start result confirms access | Use `wl_data_device`; no session sharing claim | Do not read or write portal clipboard |
| Screencast | `org.freedesktop.portal.ScreenCast` + PipeWire | `CreateSession → SelectSources → Start → OpenPipeWireRemote` succeeds with consent | Disable capture/export | No screenshot/PipeWire stream is created |
| External notifications | `org.freedesktop.Notifications` | Service owns the name and `Notify` is confirmed | Keep only Rouch’s local notification center | Do not send to an absent service |
| Clipboard | `wl_data_device_manager` | Compositor protocol binding is confirmed | Offer native Wayland only after confirmation | Copy/paste is disabled rather than faked |
| Drag-and-drop | `wl_data_device` enter/motion/drop/leave | Input and offer lifecycle are confirmed | Keep title-bar/window dragging separate | Do not accept or lose unowned drops silently |

The external notification API is a D-Bus desktop specification, not a core
Wayland protocol. Rouch’s Liquid Glass notification centre can remain local;
the bridge for applications must still own `org.freedesktop.Notifications` or
report `Degraded`/`Unavailable`.

The fixed D-Bus identifiers are available through `DbusOperation::contract()`.
They cover the real portal methods and notification calls without accepting a
caller-provided bus name, object path, interface, or member string. Dynamic
session object paths and vardict payloads are intentionally left to the real
D-Bus implementation, after the portal’s user-consent flow.

## Tahoe-like shell and low-end performance

The compatibility layer does not alter the Liquid Glass contract. It supplies
state/reason strings to the shell; the shell decides how to present a native
Wayland card, a subtle degraded badge, or a disabled action in its existing
Tahoe-inspired visual language.

The low-end rules are:

1. Detect once at startup or on an explicit refresh; never scan PATH, D-Bus
   service directories, or process state every frame.
2. Never create a GPU resource for compatibility detection.
3. Keep data offers bounded and reject invalid MIME/control input early.
4. Do not launch helpers through a shell or run continuous `busctl` probes.
5. Use native Wayland first; only start XWayland when its prerequisites exist.
6. Keep screencast/portal work user-driven and event-based; no implicit
   PipeWire connection is made by this module.

## Test and integration boundary

The module tests cover:

- missing/degraded XWayland detection and explicit native fallback;
- fixed arguments/environment and resistance to shell-like values;
- empty `PATH` components;
- bounded/deduplicated MIME offers and rejected requests;
- data-device state transitions;
- portal service-file discovery remaining degraded until live D-Bus proof;
- confirmed screencast/notification contracts and conservative local probing.

The module is registered in `src/linux.rs`. The compositor currently uses its
capability matrix at startup; the full live data-device, D-Bus notification and
portal dispatch still belongs to the native event-loop integration and must
remain degraded until those connections are actually owned by the session.

Useful standalone validation from the repository root:

```bash
rustfmt --edition 2024 --check src/linux/compat.rs
rustc --edition 2024 --test src/linux/compat.rs -o target/rouch-compat-tests
./target/rouch-compat-tests
```

The complete project `cargo check` exercises the module through that
registration; native system-library checks still need to run on Mint, Ubuntu or
Arch because this development host is Windows.
edit that owner file.

## References

- [XDG Desktop Portal API reference](https://flatpak.github.io/xdg-desktop-portal/docs/api-reference.html)
- [Portal Clipboard](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Clipboard.html)
- [Portal ScreenCast](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)
- [Portal Remote Desktop](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)
- [Desktop Notifications specification](https://specifications.freedesktop.org/notification/latest/)
