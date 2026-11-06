# 0005. Plugin GUIs floating first, then inside the window

- Status: Accepted
- Date: 2026-11-06

## Context

The owner wants plugin GUIs inside Magnolia's window, sharp at 4K, not floating as in FL Studio
([ADR-0001](0001-what-magnolia-is.md)). On Linux this is hard ([the survey](../survey.md#plugin-guis-on-wayland)):

- On Linux, VST3 and CLAP plugins take an X11 window id from the host and reparent their GUI into
  it. A Wayland program has no X11 window to give them.
- VST3 3.8 adds a preview Wayland API, in which the host acts as a small compositor for the
  plugin's surfaces. CLAP recommends floating windows on Wayland.
- Wine plugins draw into X11 windows through Xwayland.
- A Wayland client cannot place its own windows, so floating windows can be tamed only so far.
- Plugin menus and tooltips are separate X11 windows that may extend past the plugin's area.
- A Wine plugin draws at the prefix's DPI, so its sharpness at 4K depends on the plugin.

## Decision

**Floating windows first.** From Stage 1, plugin GUIs open in windows of their own, which Magnolia
opens, sizes, groups and closes. Where the compositor allows it, they stay above Magnolia's window
(`xdg-foreign` on Wayland, a transient window on X11); their placement is the compositor's. They
need no uwui. They stay afterwards as the fallback.

**Inside the window afterwards**, at a stage after the interface. Magnolia acts as a small Wayland
compositor for its plugins, built on Smithay (MIT), as the owner's compositor Umbriel is. Plugins
of the VST3 Wayland API draw into it directly; X11 and Wine plugins draw into a private Xwayland
that it runs. uwui shows each plugin's surface in an element ([uwui-needs.md](../uwui-needs.md)).
Plugin menus and tooltips become Magnolia's popups, so they are not cut at the edge.

**A generic editor beside every plugin.** Magnolia draws a list of each plugin's parameters, which
the keyboard and screen readers reach. Plugin GUIs are rarely accessible; this view is their path,
and the fallback when a GUI breaks under Wine.

## Alternatives considered

- **Inside the window from the start**: the final result at once, but nothing usable until the
  compositor and uwui exist, and no fallback when a plugin refuses.
- **Floating windows only**: much less work, but the scattered windows the owner dislikes.
- **Magnolia as an X11 program through Xwayland**, so that plugins embed as on X11: simple, but
  blurry at fractional scales and against uwui's Wayland host.

## Consequences

- The compositor is a large part of its own, planned in its stage.
- uwui needs an element that shows a surface drawn by another process
  ([uwui-needs.md](../uwui-needs.md#general-needs)).
- Sharpness of Wine plugins at 4K may need the prefix's DPI set per plugin; it is measured then.
