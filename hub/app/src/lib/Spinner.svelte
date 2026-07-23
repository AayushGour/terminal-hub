<script lang="ts">
  // Tiny inline loader. `size` in px; inherits `currentColor` so it tints to
  // whatever text color the button/label uses.
  let { size = 14 }: { size?: number } = $props();
</script>

<span
  class="spinner"
  style="width:{size}px;height:{size}px;border-width:{Math.max(2, Math.round(size / 7))}px"
  role="status"
  aria-label="loading"
></span>

<style>
  .spinner {
    display: inline-block;
    box-sizing: border-box;
    border-style: solid;
    border-color: currentColor;
    border-right-color: transparent;
    border-radius: 50%;
    animation: spin 0.6s linear infinite;
    vertical-align: -2px;
  }
  @keyframes spin {
    to { transform: rotate(360deg); }
  }
  @media (prefers-reduced-motion: reduce) {
    .spinner { animation-duration: 1.5s; }
  }
</style>
