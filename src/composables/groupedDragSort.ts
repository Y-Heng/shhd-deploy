/** 可分组条目的拖拽排序（服务器 / 隧道 / Docker 共用） */

export type DropPlace = "before" | "after" | "into";

export interface GroupedSortItem {
  id: string;
  group?: string | null;
}

export function groupOf(item: GroupedSortItem) {
  return item.group || "未分组";
}

export function groupNamesInOrder<T extends GroupedSortItem>(items: T[]): string[] {
  const names: string[] = [];
  for (const item of items) {
    const groupName = groupOf(item);
    if (!names.includes(groupName)) names.push(groupName);
  }
  return names;
}

export function dropPlaceByY(clientY: number, element: HTMLElement): "before" | "after" {
  const box = element.getBoundingClientRect();
  if (clientY < box.top + box.height / 2) return "before";
  return "after";
}

/** 命中检测时跳过正在拖的元素，避免指针捕获后永远命中手柄自己 */
export function elementFromPointIgnoringDrag(clientX: number, clientY: number): Element | null {
  const stack =
    typeof document.elementsFromPoint === "function"
      ? document.elementsFromPoint(clientX, clientY)
      : [document.elementFromPoint(clientX, clientY)];
  for (const node of stack) {
    if (node instanceof Element && !node.closest(".is-dragging")) return node;
  }
  return null;
}

/** 指针在占位条上时保持当前落点，避免布局变化导致提示闪烁 */
export function isDropPlaceholder(hit: Element | null) {
  return Boolean(hit?.closest(".drop-placeholder"));
}

/** 手柄指针拖拽：避免 WebView 里 HTML5 DnD 不触发 */
export function bindPointerDrag(
  event: PointerEvent,
  onMove: (clientX: number, clientY: number) => void,
  onUp: (clientX: number, clientY: number) => void,
) {
  if (event.button !== 0) return;
  event.preventDefault();
  event.stopPropagation();
  const grip = event.currentTarget as HTMLElement;
  const pointerId = event.pointerId;
  try {
    if (grip.setPointerCapture) grip.setPointerCapture(pointerId);
  } catch {
    // WebView 可能不支持捕获
  }
  const handleMove = (moveEvent: PointerEvent) => {
    if (moveEvent.pointerId !== pointerId) return;
    onMove(moveEvent.clientX, moveEvent.clientY);
  };
  const handleUp = (upEvent: PointerEvent) => {
    if (upEvent.pointerId !== pointerId) return;
    grip.removeEventListener("pointermove", handleMove);
    grip.removeEventListener("pointerup", handleUp);
    grip.removeEventListener("pointercancel", handleUp);
    window.removeEventListener("pointermove", handleMove);
    window.removeEventListener("pointerup", handleUp);
    window.removeEventListener("pointercancel", handleUp);
    try {
      if (grip.hasPointerCapture?.(pointerId)) grip.releasePointerCapture(pointerId);
    } catch {
      // 忽略释放失败
    }
    onUp(upEvent.clientX, upEvent.clientY);
  };
  grip.addEventListener("pointermove", handleMove);
  grip.addEventListener("pointerup", handleUp);
  grip.addEventListener("pointercancel", handleUp);
  window.addEventListener("pointermove", handleMove);
  window.addEventListener("pointerup", handleUp);
  window.addEventListener("pointercancel", handleUp);
}

/** 横向列表：指针在元素中线左侧为 before，右侧为 after */
export function dropPlaceByX(clientX: number, element: HTMLElement): "before" | "after" {
  const box = element.getBoundingClientRect();
  if (clientX < box.left + box.width / 2) return "before";
  return "after";
}

export function reorderGroups<T extends GroupedSortItem>(
  items: T[],
  fromGroup: string,
  toGroup: string,
  place: DropPlace,
): T[] {
  if (fromGroup === toGroup) return items;
  const order = groupNamesInOrder(items).filter((name) => name !== fromGroup);
  let index = order.indexOf(toGroup);
  if (index < 0) return items;
  if (place === "after") index += 1;
  order.splice(index, 0, fromGroup);
  const grouped = new Map<string, T[]>();
  for (const name of order) grouped.set(name, []);
  for (const item of items) {
    const name = groupOf(item);
    if (!grouped.has(name)) grouped.set(name, []);
    grouped.get(name)!.push(item);
  }
  const next: T[] = [];
  for (const name of order) next.push(...(grouped.get(name) ?? []));
  return next;
}

export function moveGroupedItem<T extends GroupedSortItem>(
  items: T[],
  itemId: string,
  targetGroup: string,
  targetItemId: string | null,
  place: DropPlace,
): T[] {
  const moving = items.find((item) => item.id === itemId);
  if (!moving) return items;
  const rest = items.filter((item) => item.id !== itemId);
  const clone = JSON.parse(JSON.stringify(moving)) as T;
  clone.group = targetGroup === "未分组" ? null : targetGroup;
  const order = groupNamesInOrder(items);
  if (!order.includes(targetGroup)) order.push(targetGroup);
  const grouped = new Map<string, T[]>();
  for (const name of order) grouped.set(name, []);
  for (const item of rest) {
    const name = groupOf(item);
    if (!grouped.has(name)) grouped.set(name, []);
    grouped.get(name)!.push(item);
  }
  const targetList = grouped.get(targetGroup) ?? [];
  if (place === "into" || !targetItemId) {
    targetList.push(clone);
  } else {
    let index = targetList.findIndex((item) => item.id === targetItemId);
    if (index < 0) targetList.push(clone);
    else {
      if (place === "after") index += 1;
      targetList.splice(index, 0, clone);
    }
  }
  grouped.set(targetGroup, targetList);
  const next: T[] = [];
  for (const name of order) next.push(...(grouped.get(name) ?? []));
  for (const item of rest)
    if (!next.some((row) => row.id === item.id)) next.push(item);
  return next;
}
