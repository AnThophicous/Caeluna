# Rouch Liquid Glass Tahoe contract

Rouch is a Linux desktop inspired by the compositional language of macOS
Tahoe, not a copy of Apple's proprietary assets or branding. The visual goal is
to make the whole session feel coherent: the top bar, dock, control centre,
notifications, App Gallery chrome, and window controls should belong to one
quiet material world.

## Visual hierarchy

1. The wallpaper is the ambient field and the only large colour source.
2. Windows and application content are the readable work surface. They use an
   opaque or nearly opaque backing whenever wallpaper contrast is uncertain.
3. Liquid Glass belongs to anchored shell surfaces that travel above content:
   top bar, dock, control centre, notification tray, and gallery chrome.
4. One cool edge highlight and one restrained separation shadow establish depth.
   Glow, grain, and blur never compete as separate hierarchy systems.

The persistent shell anchors orientation. Sheets descend from their anchor, the
window switcher stays centered over the current workspace, and minimized
windows settle toward their dock group. These relationships must still read
with blur and animation disabled.

## Geometry and type

- 8px base rhythm; 12px controls; 16px panel padding; 24px region separation.
- 10px controls, 12px windows/content frames, 18px transient shell sheets.
- Circles are reserved for avatars, status dots, and traffic-light actions.
- System UI fallback typography uses a 30/22px display scale, 22px page titles,
  15px body text, 13px controls, and 11px metadata.
- Primary pointer and keyboard targets aim for 44 logical px where space allows;
  no essential action is hover-only.

## Performance tiers

The same hierarchy must survive three renderer states:

| Tier | Material | Motion | Intended machine |
| --- | --- | --- | --- |
| Tahoe | bounded backdrop sampling on approved shell surfaces | short spatial transitions | capable GPU |
| Balanced | tinted glass with reduced sampling and capped blur | reduced scale and opacity transitions | normal integrated GPU |
| Saver | opaque cool-tinted surfaces, no backdrop blur | discrete state changes and immediate settlement | older/low-end PC |

The compositor must never blur the full output, allocate per frame for a
transient panel, or keep an off-screen effect loop running. Reduced
transparency, battery saver, context loss, and unavailable GPU features all
select a designed opaque surface rather than removing boundaries.

## Tahoe-like interaction acceptance

- The top bar and dock remain visible enough to orient, but never cover focused
  content or keyboard focus.
- Every sheet opens from a stable anchor, can be interrupted, and closes with
  Escape or an outside press where appropriate.
- Alt-Tab preserves MRU order and visibly settles on the selected window.
- Notifications announce unread state without stealing focus; DND is explicit.
- App Gallery always shows source and install/launch state; offline and missing
  Flatpak are recoverable states.
- Mint, Ubuntu, and Arch are the official native-session targets. Other systems
  use a clearly labeled best-effort or nested path.

## Visual QA gate

Before calling a surface Tahoe-like, inspect it in populated, empty, offline,
error, reduced-transparency, reduced-motion, narrow, and intermediate-width
states. Also inspect grayscale, effects-off, and logo-off: the composition,
hierarchy, and shell anchors must remain identifiable without decoration.
