/**
 * ZMODEM (lrzsz) 文件传输支持模块
 *
 * 当终端收到 rz / sz 命令触发的 ZMODEM 协议特征码时，
 * 由 Sentry 拦截并管理 ZmodemSession，通过 Tauri 本地文件对话框及流式读写完成上传/下载。
 */

import * as Zmodem from 'zmodem.js/src/zmodem_browser.js';
import type { Terminal } from '@xterm/xterm';
import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog';
import { api } from './api';

export interface ZmodemSessionController {
  consume: (bytes: Uint8Array) => void;
  dispose: () => void;
  isTransferring: () => boolean;
}

export interface ZmodemHandlerOptions {
  sessionId: string;
  terminal: Terminal;
  onTransferStart?: (direction: 'upload' | 'download') => void;
  onTransferEnd?: () => void;
}

interface ZmodemDetection {
  confirm: () => ZmodemSessionInstance;
  deny: () => void;
}

interface ZmodemSessionInstance {
  type: 'send' | 'receive';
  abort: () => void;
  close: () => Promise<void>;
  start: () => Promise<void>;
  send_offer: (params: Record<string, unknown>) => Promise<ZmodemTransfer | undefined>;
  on: (event: string, handler: (...args: unknown[]) => void) => void;
  _create_header_bytes?: (name_and_args: unknown[]) => unknown[];
}

interface ZmodemTransfer {
  get_offset: () => number;
  get_details: () => { name?: string; size?: number };
  on: (event: string, handler: () => void) => void;
  skip: () => void;
  accept: () => Promise<Uint8Array[]>;
  send: (piece: Uint8Array) => void;
  end: (piece: Uint8Array) => Promise<void>;
}

interface ZmodemSentryInstance {
  consume: (bytes: Uint8Array | number[]) => void;
}

interface ZmodemSentryConstructor {
  new (options: {
    to_terminal: (octets: number[] | Uint8Array) => void;
    sender: (octets: number[] | Uint8Array) => void;
    on_retract: () => void;
    on_detect: (detection: ZmodemDetection) => void;
  }): ZmodemSentryInstance;
}

/** 格式化字节大小 */
function formatSize(bytes: number): string {
  if (bytes <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  const i = Math.floor(Math.log(bytes) / Math.log(1024));
  return `${(bytes / Math.pow(1024, i)).toFixed(2)} ${units[i]}`;
}

interface UploadFileItem {
  name: string;
  size: number;
  data: Uint8Array;
  mtime: Date;
}

/**
 * 自定义分块上传实现：
 * 1. 规避浏览器中缺少 onprogress 或 FileReader 一次性读取导致的无进度问题；
 * 2. 真实按 8KB~64KB 切片发送并即时更新终端进度条；
 * 3. 支持多文件批量顺序传输与覆盖模式（ZF1_ZMCLOB）。
 */
async function sendFilesWithProgress(
  session: ZmodemSessionInstance,
  files: UploadFileItem[],
  terminal: Terminal
): Promise<void> {
  const CHUNK_SIZE = 8192; // 8 KB 每块

  for (let fileIndex = 0; fileIndex < files.length; fileIndex++) {
    const file = files[fileIndex];
    terminal.write(`\r\n\x1b[36m[ZMODEM] 正在发送 (${fileIndex + 1}/${files.length}): ${file.name} (${formatSize(file.size)})\x1b[0m\r\n`);

    const offerParams = {
      name: file.name,
      size: file.size,
      mtime: file.mtime,
      files_remaining: files.length - fileIndex,
      bytes_remaining: files.slice(fileIndex).reduce((acc, cur) => acc + cur.size, 0),
    };

    const xfer = await session.send_offer(offerParams);
    if (!xfer) {
      terminal.write(`\r\x1b[K\x1b[33m[ZMODEM] 远端跳过文件: ${file.name}\x1b[0m\r\n`);
      continue;
    }

    const total = file.size;
    let offset = xfer.get_offset();
    const data = file.data;

    // 分块发送文件内容
    while (offset + CHUNK_SIZE < total) {
      const piece = data.subarray(offset, offset + CHUNK_SIZE);
      xfer.send(piece);
      offset += piece.length;

      const percent = total > 0 ? Math.min(100, Math.floor((offset / total) * 100)) : 100;
      terminal.write(`\r\x1b[K[ZMODEM] ${file.name}: ${percent}% (${formatSize(offset)} / ${formatSize(total)})`);

      // 让出事件循环，保证终端即时刷新与 PTY 发送缓冲
      await new Promise((resolve) => setTimeout(resolve, 0));
    }

    // 发送最后一块并结束此文件
    const lastPiece = data.subarray(offset, total);
    await xfer.end(lastPiece);
    terminal.write(`\r\x1b[K\x1b[32m[ZMODEM] 已完成: ${file.name} (100%)\x1b[0m\r\n`);
  }
}

export function createZmodemSession(options: ZmodemHandlerOptions): ZmodemSessionController {
  const { sessionId, terminal, onTransferStart, onTransferEnd } = options;

  let activeSession: ZmodemSessionInstance | null = null;
  let isTransferActive = false;

  const sender = (octets: number[] | Uint8Array) => {
    api.terminalWriteRaw(sessionId, octets).catch((err) => {
      console.error('[ZMODEM] 写入远端失败:', err);
    });
  };

  const toTerminal = (octets: number[] | Uint8Array) => {
    const bytes = octets instanceof Uint8Array ? octets : new Uint8Array(octets);
    terminal.write(bytes);
  };

  const reset = () => {
    if (isTransferActive) {
      isTransferActive = false;
      terminal.options.disableStdin = false;
      terminal.focus();
      onTransferEnd?.();
    }
    activeSession = null;
  };

  // 处理上传（用户执行 rz 时，远端发送 ZRINIT，此时 session.type === "send"）
  const handleSendSession = async (session: ZmodemSessionInstance) => {
    isTransferActive = true;
    activeSession = session;
    terminal.options.disableStdin = true;
    onTransferStart?.('upload');

    // 启用覆盖模式（ZF1 = 4 / ZF1_ZMCLOB），使服务端 rz 强制覆盖同名文件而不是跳过
    const origCreateHeaderBytes = session._create_header_bytes;
    if (origCreateHeaderBytes) {
      session._create_header_bytes = function (name_and_args: unknown[]) {
        const res = origCreateHeaderBytes.call(this, name_and_args) as [number[], { _bytes4: number[] }];
        if (name_and_args && name_and_args[0] === 'ZFILE' && res && res[1] && res[1]._bytes4) {
          res[1]._bytes4[2] = 4; // ZF1_ZMCLOB: 强制覆盖
          const formatter = (this as unknown as { _get_header_formatter: (name: unknown) => string })._get_header_formatter(name_and_args[0]);
          res[0] = (res[1] as unknown as Record<string, (enc: unknown) => number[]>)[formatter](
            (this as unknown as { _zencoder: unknown })._zencoder
          );
        }
        return res;
      };
    }

    session.on('session_end', () => {
      reset();
    });

    try {
      // 打开文件弹窗前和后确保鼠标指针显示
      await api.restoreMouseCursor().catch(() => {});
      const selected = await openDialog({
        title: '选择要上传的文件（rz）',
        multiple: true,
      });
      await api.restoreMouseCursor().catch(() => {});
      if (!selected) {
        // 用户取消了选择，向远端发送中止序列
        terminal.write('\r\n\x1b[33m[ZMODEM] 取消上传\x1b[0m\r\n');
        try {
          session.abort();
        } catch {
          // ignore
        }
        reset();
        return;
      }

      const paths = Array.isArray(selected) ? selected : [selected];
      if (paths.length === 0) {
        session.abort();
        reset();
        return;
      }

      // 读取本地文件二进制内容
      const uploadFiles: UploadFileItem[] = [];
      for (const p of paths) {
        const bytes = await api.readLocalFile(p);
        const u8 = new Uint8Array(bytes);
        const fileName = p.replace(/\\/g, '/').split('/').pop() || 'upload.bin';
        uploadFiles.push({
          name: fileName,
          size: u8.length,
          data: u8,
          mtime: new Date(),
        });
      }

      terminal.write(`\r\n\x1b[32m[ZMODEM] 准备上传 ${uploadFiles.length} 个文件（已开启同名覆盖）…\x1b[0m\r\n`);

      // 使用自定义的分块流式上传，保证精准进度反馈与同名覆盖
      await sendFilesWithProgress(session, uploadFiles, terminal);

      await session.close();
      terminal.write('\x1b[32m[ZMODEM] 所有文件传输完成\x1b[0m\r\n');
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : String(err);
      terminal.write(`\r\n\x1b[31m[ZMODEM] 上传错误: ${msg}\x1b[0m\r\n`);
      try {
        session.abort();
      } catch {
        // ignore
      }
    } finally {
      reset();
    }
  };

  // 处理下载（用户执行 sz 时，远端发送 ZRQINIT，此时 session.type === "receive"）
  const handleReceiveSession = async (session: ZmodemSessionInstance) => {
    isTransferActive = true;
    activeSession = session;
    terminal.options.disableStdin = true;
    onTransferStart?.('download');

    session.on('session_end', () => {
      reset();
    });

    session.on('offer', async (...args: unknown[]) => {
      const xfer = args[0] as ZmodemTransfer;
      const details = xfer.get_details();
      const fileName = details.name || 'download.bin';
      const fileSize = details.size || 0;

      terminal.write(`\r\n\x1b[36m[ZMODEM] 接收文件提议: ${fileName} (${formatSize(fileSize)})\x1b[0m\r\n`);

      // 选择本地保存路径
      let savePath: string | null = null;
      try {
        await api.restoreMouseCursor().catch(() => {});
        savePath = await saveDialog({
          title: `保存文件: ${fileName}`,
          defaultPath: fileName,
        });
        await api.restoreMouseCursor().catch(() => {});
      } catch (err) {
        console.error('[ZMODEM] 打开保存对话框失败:', err);
      }

      if (!savePath) {
        terminal.write(`\r\n\x1b[33m[ZMODEM] 用户拒绝保存: ${fileName}\x1b[0m\r\n`);
        xfer.skip();
        return;
      }

      xfer.on('input', () => {
        const offset = xfer.get_offset();
        const percent = fileSize > 0 ? Math.min(100, Math.floor((offset / fileSize) * 100)) : 100;
        terminal.write(`\r\x1b[K[ZMODEM] 接收 ${fileName}: ${percent}% (${formatSize(offset)} / ${formatSize(fileSize)})`);
      });

      try {
        const payloads = await xfer.accept();
        terminal.write(`\r\x1b[K\x1b[32m[ZMODEM] 接收完成，正在写入本地磁盘: ${savePath}…\x1b[0m\r\n`);

        // 将 payloads 合并成单一 Uint8Array
        let totalLen = 0;
        for (const p of payloads) totalLen += p.length;
        const merged = new Uint8Array(totalLen);
        let offset = 0;
        for (const p of payloads) {
          merged.set(p, offset);
          offset += p.length;
        }

        await api.writeLocalFile(savePath, merged);
        terminal.write(`\x1b[32m[ZMODEM] 保存成功: ${fileName} (${formatSize(merged.length)})\x1b[0m\r\n`);
      } catch (err: unknown) {
        const msg = err instanceof Error ? err.message : String(err);
        terminal.write(`\r\n\x1b[31m[ZMODEM] 接收保存失败: ${msg}\x1b[0m\r\n`);
        try {
          session.abort();
        } catch {
          // ignore
        }
      }
    });

    try {
      await session.start();
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : String(err);
      terminal.write(`\r\n\x1b[31m[ZMODEM] 传输中断: ${msg}\x1b[0m\r\n`);
    } finally {
      reset();
    }
  };

  const SentryClass = Zmodem.Sentry as unknown as ZmodemSentryConstructor;
  const sentry = new SentryClass({
    to_terminal: toTerminal,
    sender: sender,
    on_retract: () => {
      reset();
    },
    on_detect: (detection: ZmodemDetection) => {
      const session = detection.confirm();
      if (session.type === 'send') {
        handleSendSession(session);
      } else {
        handleReceiveSession(session);
      }
    },
  });

  return {
    consume: (bytes: Uint8Array) => {
      try {
        sentry.consume(bytes);
      } catch (err) {
        console.error('[ZMODEM] consume 异常:', err);
        reset();
      }
    },
    dispose: () => {
      if (activeSession) {
        try {
          activeSession.abort();
        } catch {
          // ignore
        }
      }
      reset();
    },
    isTransferring: () => isTransferActive,
  };
}
