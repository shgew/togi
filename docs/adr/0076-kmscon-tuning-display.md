# kmscon is the optional tuning-boot display

The tuning boot may run its terminal dashboard in kmscon, directly on KMS/DRM, rather than in the Linux console. The console remains the default and the fallback. The [runtime spec](../spec/runtime.md#tuning-boot) owns configuration, service isolation, keyboard lockdown and failure recovery; the [howto](../howto.md#4-overnight-the-tuning-boot) covers setup and monitor selection.

kmscon supplies truecolour and Unicode without putting a compositor in the tuning boot. A target-machine probe demonstrated the GLES drm3d/gltex path on amdgpu. It measured about 2% of one core while writing 30 frames per second and about 0.1% for a static dashboard; these are probe results, not a reason to accelerate the dashboard's refresh timer. The dashboard continues to use its existing palette, without new colours, gradients or animation. DejaVu Sans Mono covers its glyphs and is shipped with the display.

## Alternatives rejected

- Cage plus a terminal emulator adds a Wayland compositor, the terminal's rendering stack and a larger closure for one fullscreen terminal. Its additional processes and rendering activity create more opportunities to contaminate core measurements without adding a needed capability.
- A general Wayland kiosk adds session, compositor and output-management machinery that this unattended terminal does not need. kmscon keeps those concerns out of the tuning runtime, at the cost of cloning all connected outputs rather than selecting a named connector.
- Replacing the console everywhere would make GPU startup a requirement for unattended visibility. Keeping the console as the default and switching back after repeated kmscon failures preserves a simpler recovery path. Neither display controls tuning, and no display hang detector is introduced.

The display uses the same root identity as the existing console watcher: kmscon needs direct VT/DRM access, and its watcher child gains no additional privilege. Restricting privileges for both watcher variants is a separate decision, not an implicit reason to give one variant different journal access.
