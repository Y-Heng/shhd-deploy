import { createApp } from "vue";
import ElementPlus from "element-plus";
import zhCn from "element-plus/es/locale/lang/zh-cn";
import "element-plus/dist/index.css";
import "element-plus/theme-chalk/dark/css-vars.css";
import "./styles/theme.css";
import App from "./App.vue";

// 启用暗色主题
document.documentElement.classList.add("dark");

const app = createApp(App);
app.use(ElementPlus, { locale: zhCn });
app.mount("#app");
