<!-- SPDX-License-Identifier: Apache-2.0 -->
<script lang="ts">
  import type { Category } from "@vayu/api-client/categories";
  import Icon from "./Icon.svelte";
  import type { IconName } from "./icons.ts";

  interface Props {
    category: Category;
    size?: "sm" | "md" | "lg";
  }
  let { category, size = "md" }: Props = $props();

  const GLYPH: Record<Category, IconName> = {
    video: "video",
    music: "music",
    documents: "document",
    compressed: "archive",
    programs: "app",
    images: "image",
    other: "file",
  };
  const PX = { sm: 14, md: 18, lg: 22 } as const;
</script>

<span class="tile {size}" style:--tint="var(--vy-cat-{category})" aria-hidden="true">
  <Icon name={GLYPH[category]} size={PX[size]} />
</span>

<style>
  .tile {
    display: inline-grid;
    place-items: center;
    flex: none;
    color: var(--tint);
    background: color-mix(in srgb, var(--tint) 13%, transparent);
    border-radius: var(--vy-radius-sm);
  }
  .sm {
    inline-size: 22px;
    block-size: 22px;
    border-radius: var(--vy-radius-xs);
  }
  .md {
    inline-size: 32px;
    block-size: 32px;
  }
  .lg {
    inline-size: 42px;
    block-size: 42px;
    border-radius: var(--vy-radius-md);
  }
  :global([data-theme="high-contrast"]) .tile {
    background: transparent;
    outline: 1px solid var(--tint);
  }
</style>
