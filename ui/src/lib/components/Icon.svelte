<script lang="ts" module>
  // Lucide-style 16px icons, 1.5 stroke (DESIGN §3.5). Paths only — rendered inline.
  const PATHS: Record<string, string> = {
    "chev-d": '<path d="M4 6l4 4 4-4"/>',
    "chev-r": '<path d="M6 4l4 4-4 4"/>',
    menu: '<path d="M3 4h10M3 8h10M3 12h10"/>',
    search: '<circle cx="7" cy="7" r="4"/><path d="M10 10l3 3"/>',
    plus: '<path d="M8 3v10M3 8h10"/>',
    gear: '<circle cx="8" cy="8" r="2"/><path d="M8 1.8v1.7M8 12.5v1.7M1.8 8h1.7M12.5 8h1.7M3.6 3.6l1.2 1.2M11.2 11.2l1.2 1.2M3.6 12.4l1.2-1.2M11.2 4.8l1.2-1.2"/>',
    cloud: '<path d="M4.5 12.5h7a2.5 2.5 0 0 0 .3-5A3.5 3.5 0 0 0 5 6.6a3 3 0 0 0-.5 5.9z"/><path d="M6.5 9.5l1.2 1.2 2-2.2"/>',
    "cloud-off": '<path d="M4.5 12.5h7a2.5 2.5 0 0 0 .3-5A3.5 3.5 0 0 0 5 6.6a3 3 0 0 0-.5 5.9z"/><path d="M6.5 8.5l3 3M9.5 8.5l-3 3"/>',
    import: '<path d="M8 2.5v7M5 7l3 3 3-3M3 12.5h10"/>',
    more: '<circle cx="3.5" cy="8" r=".6"/><circle cx="8" cy="8" r=".6"/><circle cx="12.5" cy="8" r=".6"/>',
    copy: '<rect x="5.5" y="5.5" width="8" height="8" rx="1.5"/><path d="M10.5 5.5V4a1.5 1.5 0 0 0-1.5-1.5H4A1.5 1.5 0 0 0 2.5 4v5A1.5 1.5 0 0 0 4 10.5h1.5"/>',
    save: '<path d="M8 2.5v8M5 7.5l3 3 3-3M3 13.5h10"/>',
    cols: '<rect x="2.5" y="3" width="11" height="10" rx="1.5"/><path d="M8 3v10"/>',
    rows: '<rect x="2.5" y="3" width="11" height="10" rx="1.5"/><path d="M2.5 8h11"/>',
    folder: '<path d="M2.5 4.5a1 1 0 0 1 1-1h3l1.5 1.5h4.5a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1z"/>',
    stack: '<path d="M8 2.5l5.5 3-5.5 3-5.5-3z"/><path d="M2.5 8.5l5.5 3 5.5-3"/>',
    x: '<path d="M4 4l8 8M12 4l-8 8"/>',
    check: '<path d="M3.5 8.5l3 3 6-7"/>',
    lock: '<rect x="3.5" y="7" width="9" height="6.5" rx="1.5"/><path d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2"/>',
    unlock: '<rect x="3.5" y="7" width="9" height="6.5" rx="1.5"/><path d="M5.5 7V5a2.5 2.5 0 0 1 4.8-1"/>',
    info: '<circle cx="8" cy="8" r="5.5"/><path d="M8 7.5v3.5M8 5.2v.1"/>',
    alert: '<path d="M8 2.5l6 10.5H2z"/><path d="M8 6.5v3M8 11.3v.1"/>',
    layers: '<rect x="2.5" y="2.5" width="11" height="11" rx="2"/><path d="M2.5 6.5h11"/>',
    trash: '<path d="M3 4.5h10M6.5 4.5V3h3v1.5M4.5 4.5l.6 8.5h5.8l.6-8.5"/>',
    eye: '<path d="M1.5 8s2.4-4.5 6.5-4.5S14.5 8 14.5 8 12.1 12.5 8 12.5 1.5 8 1.5 8z"/><circle cx="8" cy="8" r="1.8"/>',
    "eye-off": '<path d="M2 2l12 12M6.2 4A6.5 6.5 0 0 1 8 3.5c4.1 0 6.5 4.5 6.5 4.5a11 11 0 0 1-1.8 2.3M4.2 5.3A11 11 0 0 0 1.5 8S3.9 12.5 8 12.5a6 6 0 0 0 2.6-.6"/>',
    globe: '<circle cx="8" cy="8" r="5.5"/><path d="M2.5 8h11M8 2.5c1.7 1.6 2.5 3.4 2.5 5.5S9.7 11.9 8 13.5C6.3 11.9 5.5 10.1 5.5 8S6.3 4.1 8 2.5z"/>',
    clock: '<circle cx="8" cy="8" r="5.5"/><path d="M8 5v3l2 1.5"/>',
    sliders: '<path d="M3 4.5h6M12 4.5h1M3 11.5h1M7 11.5h6"/><circle cx="10.5" cy="4.5" r="1.5"/><circle cx="5.5" cy="11.5" r="1.5"/>',
    link: '<path d="M7 9a2.5 2.5 0 0 0 3.5 0l2-2A2.5 2.5 0 0 0 9 3.5l-.7.7M9 7a2.5 2.5 0 0 0-3.5 0l-2 2A2.5 2.5 0 0 0 7 12.5l.7-.7"/>',
    share: '<path d="M8 10V2.5M5 5.5l3-3 3 3M3.5 8.5v4a1 1 0 0 0 1 1h7a1 1 0 0 0 1-1v-4"/>',
    file: '<path d="M4 2.5h5l3 3v8H4z"/><path d="M9 2.5v3h3"/>',
    refresh: '<path d="M13 4.5V7h-2.5M3 11.5V9h2.5"/><path d="M12.6 7A4.8 4.8 0 0 0 4 5.2M3.4 9A4.8 4.8 0 0 0 12 10.8"/>',
    panel: '<rect x="2.5" y="3" width="11" height="10" rx="1.5"/><path d="M6 3v10"/>',
    edit: '<path d="M10.5 3l2.5 2.5-7 7H3.5V10z"/>',
    "check-circle": '<circle cx="8" cy="8" r="5.5"/><path d="M5.5 8.2l1.8 1.8 3.2-3.6"/>',
    "x-circle": '<circle cx="8" cy="8" r="5.5"/><path d="M6 6l4 4M10 6l-4 4"/>',
    upload: '<path d="M8 10.5v-8M5 5.5l3-3 3 3M3 13.5h10"/>',
  };
</script>

<script lang="ts">
  let { name, small = false, label }: { name: string; small?: boolean; label?: string } = $props();
</script>

<svg
  class="i"
  class:sm={small}
  viewBox="0 0 16 16"
  aria-hidden={label ? undefined : "true"}
  aria-label={label}
  role={label ? "img" : undefined}
>
  {@html PATHS[name] ?? ""}
</svg>
