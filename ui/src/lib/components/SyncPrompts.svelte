<script lang="ts">
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { foreignSpaces, removedSpaces } from "../util/spaces";

  /**
   * Opens the personal-sync dialogs when sync asks for a decision, once no other modal is open:
   * the first-sync checklist after a first sign-in (UX §10), and the space notice for spaces
   * paused for another account (UX §4.3) or removed (UX §6.9). Each opens once per session;
   * after that Settings › Sync offers "Decide".
   */
  let firstSyncAsked = false;
  const noticed = new Set<string>();

  $effect(() => {
    if (!app.ready || !app.signedIn || ui.anyModalOpen) return;
    // A paused space holding no projects needs no decision (e.g. B's empty Personal after A is back).
    const foreign = foreignSpaces(app.sync).filter((s) => s.projectCount > 0 && !noticed.has(s.spaceId));
    if (foreign.length) {
      for (const s of foreign) noticed.add(s.spaceId);
      ui.spaceNotice = { mode: "foreign", spaceIds: foreign.map((s) => s.spaceId) };
      return;
    }
    const removed = removedSpaces(app.sync).filter((s) => !noticed.has(s.spaceId));
    if (removed.length) {
      for (const s of removed) noticed.add(s.spaceId);
      ui.spaceNotice = { mode: "removed", spaceIds: removed.map((s) => s.spaceId) };
      return;
    }
    if (app.sync.firstSyncPending && !firstSyncAsked) {
      firstSyncAsked = true;
      ui.firstSync = true;
    }
  });
</script>
