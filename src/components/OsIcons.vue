<script setup lang="ts">
/** 按探测到的系统类型画操作系统图标 */
import { computed } from "vue";
import type { DetectedOs } from "../types";

const props = defineProps<{
  os: DetectedOs | string;
  size?: number;
}>();

const iconSize = computed(() => props.size ?? 24);

const kind = computed(() => {
  if (props.os === "windows") return "windows";
  if (props.os === "ubuntu") return "ubuntu";
  if (props.os === "centos") return "centos";
  return "linux";
});
</script>

<template>
  <!-- Windows：四格窗 -->
  <svg
    v-if="kind === 'windows'"
    class="os-icon"
    :width="iconSize"
    :height="iconSize"
    viewBox="0 0 24 24"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <rect x="2.5" y="2.5" width="8.5" height="8.5" rx="1" fill="currentColor" />
    <rect x="13" y="2.5" width="8.5" height="8.5" rx="1" fill="currentColor" />
    <rect x="2.5" y="13" width="8.5" height="8.5" rx="1" fill="currentColor" />
    <rect x="13" y="13" width="8.5" height="8.5" rx="1" fill="currentColor" />
  </svg>

  <!-- Ubuntu：圆圈 + 三点 -->
  <svg
    v-else-if="kind === 'ubuntu'"
    class="os-icon"
    :width="iconSize"
    :height="iconSize"
    viewBox="0 0 24 24"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <circle cx="12" cy="12" r="8.2" stroke="currentColor" stroke-width="1.8" />
    <circle cx="12" cy="4.2" r="2.1" fill="currentColor" />
    <circle cx="5.3" cy="16.2" r="2.1" fill="currentColor" />
    <circle cx="18.7" cy="16.2" r="2.1" fill="currentColor" />
    <path
      d="M12 6.4v2.2M7.3 15.1l1.9-1.1M16.7 15.1l-1.9-1.1"
      stroke="currentColor"
      stroke-width="1.4"
      stroke-linecap="round"
    />
  </svg>

  <!-- CentOS：简化山形徽章 -->
  <svg
    v-else-if="kind === 'centos'"
    class="os-icon"
    :width="iconSize"
    :height="iconSize"
    viewBox="0 0 24 24"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <path
      d="M12 3.2L20.2 8v8L12 20.8 3.8 16V8L12 3.2Z"
      stroke="currentColor"
      stroke-width="1.6"
      stroke-linejoin="round"
    />
    <path
      d="M12 7.2L16.6 10v4L12 16.8 7.4 14v-4L12 7.2Z"
      fill="currentColor"
      opacity="0.9"
    />
    <path
      d="M12 9.2v5.6M9.2 12h5.6"
      stroke="#000"
      stroke-opacity="0.25"
      stroke-width="1.2"
      stroke-linecap="round"
    />
  </svg>

  <!-- Linux：简笔企鹅 -->
  <svg
    v-else
    class="os-icon"
    :width="iconSize"
    :height="iconSize"
    viewBox="0 0 24 24"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <ellipse cx="12" cy="14.2" rx="6.2" ry="7" fill="currentColor" />
    <circle cx="12" cy="6.6" r="3.4" fill="currentColor" />
    <ellipse cx="12" cy="15.2" rx="3.4" ry="4.2" fill="#fff" opacity="0.92" />
    <circle cx="10.6" cy="6.2" r="0.7" fill="#1a2130" />
    <circle cx="13.4" cy="6.2" r="0.7" fill="#1a2130" />
    <path
      d="M10.8 7.6c.4.5 1.2.8 2.2.8s1.8-.3 2.2-.8"
      stroke="#1a2130"
      stroke-width="0.9"
      stroke-linecap="round"
    />
    <path
      d="M7.2 18.8c-.8.6-1.8 1-2.8 1.1M16.8 18.8c.8.6 1.8 1 2.8 1.1"
      stroke="currentColor"
      stroke-width="1.4"
      stroke-linecap="round"
    />
  </svg>
</template>

<style scoped>
.os-icon {
  display: block;
  flex-shrink: 0;
}
</style>
