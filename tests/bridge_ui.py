"""Browser smoke against the real v2 daemon. Vite must be running on :1420.

Only Tauri's desktop transport/file picker is substituted; business responses
come from the actual Rust service and SQLite database.
"""
import asyncio
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import shutil

from playwright.async_api import async_playwright
from bridge_runtime import rpc, wait_until


async def main():
    binary = Path(sys.argv[1]).resolve()
    output = Path("/tmp/sessionbridge-ui-review")
    output.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="sb-ui-") as temporary:
        directory = Path(temporary)
        root, state = directory / "SessionBridge demo", directory / "state"
        root.mkdir()
        service = subprocess.Popen([str(binary), "serve"], env=dict(os.environ, SESSIONBRIDGE_V2_STATE=str(state)), stderr=subprocess.PIPE)
        try:
            await asyncio.to_thread(wait_until, lambda: (state / "bridge.sock").exists())
            async with async_playwright() as playwright:
                browser = await playwright.chromium.launch(executable_path=shutil.which("chromium"), headless=True)
                page = await browser.new_page(viewport={"width": 1380, "height": 860}, reduced_motion="reduce")
                errors = []
                page.on("pageerror", lambda error: errors.append(str(error)))

                async def invoke(command, args):
                    if command == "plugin:dialog|open":
                        return str(root)
                    if command != "bridge_call":
                        raise AssertionError(command)
                    return await asyncio.to_thread(rpc, state, args["workspace"] or root, args["operation"], args["args"])

                await page.expose_function("bridgeInvoke", invoke)
                await page.add_init_script("window.__TAURI_INTERNALS__ = {invoke: (command, args) => window.bridgeInvoke(command, args)}")
                await page.goto("http://127.0.0.1:1420")
                await page.get_by_role("button", name="打开项目", exact=True).click()
                await page.wait_for_timeout(700)
                if errors:
                    raise AssertionError(errors)
                alerts = await page.get_by_role("alert").all_text_contents()
                assert not alerts, alerts
                await page.get_by_role("button", name="新建工作群", exact=True).first.click()
                await page.get_by_label("名称", exact=True).fill("重构登录流程")
                await page.get_by_role("button", name="创建", exact=True).click()
                await page.get_by_role("heading", name="成员", exact=True).wait_for()
                await page.get_by_role("button", name="添加执行者", exact=True).click()
                await page.get_by_text("执行者 2", exact=True).wait_for()
                await page.get_by_role("button", name="关闭", exact=True).click()
                await page.get_by_role("textbox", name="消息", exact=True).fill("先梳理认证流程，再实现并验证修复。")
                await page.get_by_role("button", name="发送消息", exact=True).click()
                await page.get_by_text("先梳理认证流程，再实现并验证修复。", exact=True).wait_for()
                await page.get_by_role("button", name="@ 协调者", exact=False).click()
                await page.get_by_role("button", name="所有人", exact=True).click()
                await page.get_by_role("button", name="完成", exact=True).click()
                await page.get_by_role("textbox", name="消息", exact=True).fill("所有成员先确认各自负责的范围。")
                await page.get_by_role("button", name="发送消息", exact=True).click()
                await page.get_by_text("所有成员先确认各自负责的范围。", exact=True).wait_for()
                await page.get_by_role("textbox", name="消息", exact=True).fill("尚未发送的补充说明")
                await page.reload()
                await page.get_by_role("textbox", name="消息", exact=True).wait_for()
                assert await page.get_by_role("textbox", name="消息", exact=True).input_value() == "尚未发送的补充说明"
                for width in (1380, 1024, 390):
                    await page.set_viewport_size({"width": width, "height": 860})
                    await page.screenshot(path=str(output / f"group-{width}.png"), full_page=True)
                    overflow = await page.evaluate("document.documentElement.scrollWidth-document.documentElement.clientWidth")
                    assert overflow == 0, (width, overflow)
                await page.get_by_role("button", name="成员", exact=True).click()
                await page.screenshot(path=str(output / "members-390.png"), full_page=True)
                await page.keyboard.press("Escape")
                await page.get_by_role("button", name="成员", exact=True).wait_for()
                assert not errors, errors
                await browser.close()
                print(f"PASS: real backend, create/add/send/broadcast/draft restore; overflow=0 at 390/1024/1380; screenshots {output}")
        finally:
            service.terminate()
            service.communicate(timeout=10)


if __name__ == "__main__":
    asyncio.run(main())
