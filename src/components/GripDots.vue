<!-- 拖拽排序手柄：两列圆点 -->
<template>
  <span class="grip-dots" aria-hidden="true">
    <span v-for="dotIndex in 8" :key="dotIndex" class="grip-dot" />
  </span>
</template>

<style scoped>
.grip-dots {
  display: grid;
  grid-template-columns: 3px 3px;
  grid-auto-rows: 3px;
  gap: 2px 3px;
  width: 9px;
  height: 18px;
}
.grip-dot {
  width: 3px;
  height: 3px;
  border-radius: 50%;
  background: currentColor;
}
</style>
