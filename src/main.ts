/** 前端入口：独立终端弹出窗走 TermPopout，否则进主界面 */

import { createApp } from "vue";
import ElementPlus from "element-plus";
import zhCn from "element-plus/es/locale/lang/zh-cn";
import "element-plus/dist/index.css";
import "element-plus/theme-chalk/dark/css-vars.css";
import "./styles/theme.css";
import App from "./App.vue";
import TermPopout from "./views/TermPopout.vue";
import { parsePopoutHash } from "./sshTerminal";
import { initAppearance } from "./composables/useAppearance";

initAppearance();

const popout = parsePopoutHash();
const app = popout
  ? createApp(TermPopout, popout)
  : createApp(App);
app.use(ElementPlus, { locale: zhCn });
app.mount("#app");
