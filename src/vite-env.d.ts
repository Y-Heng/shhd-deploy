/// <reference types="vite/client" />

/** Vue SFC 与 Element Plus 中文语言包的类型声明 */

declare module "*.vue" {
  import type { DefineComponent } from "vue";
  const component: DefineComponent<{}, {}, any>;
  export default component;
}

declare module "element-plus/es/locale/lang/zh-cn";
