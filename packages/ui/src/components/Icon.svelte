<!-- SPDX-License-Identifier: Apache-2.0 -->
<script lang="ts">
  import { DIRECTIONAL, DOTTED, ICONS, type IconName } from "./icons.ts";

  interface Props {
    name: IconName;
    size?: number;
    /** Give the icon an accessible name; without one it is hidden from assistive tech. */
    label?: string;
  }
  let { name, size = 16, label }: Props = $props();
</script>

<svg
  class="icon"
  class:flip={DIRECTIONAL.has(name)}
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width={DOTTED.has(name) ? 3 : 1.75}
  stroke-linecap="round"
  stroke-linejoin="round"
  focusable="false"
  role={label ? "img" : undefined}
  aria-label={label}
  aria-hidden={label ? undefined : "true"}
>
  <path d={ICONS[name]} />
</svg>

<style>
  .icon {
    flex: none;
    display: block;
  }
  :global([dir="rtl"]) .flip {
    transform: scaleX(-1);
  }
</style>
