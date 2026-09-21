"""A stateless extension: each interaction is a new process; stdlib only."""
import json
import sys


def widget(message="请选择文档，或输入一段文字。"):
    return {
        "type": "set_widget", "id": "demo", "placement": "above_editor",
        "blocks": [
            {"type": "markdown", "text": "**交互扩展示例**\n" + message},
            {"type": "select", "id": "document", "label": "文档", "options": [
                {"id": "install", "label": "安装说明"},
                {"id": "develop", "label": "开发指南"},
            ]},
            {"type": "input", "id": "message", "label": "留言", "value": ""},
            {"type": "button", "id": "confirm", "label": "打开确认弹窗"},
            {"type": "button", "id": "reset", "label": "重置组件"},
        ],
    }


def handle(request):
    if not request.get("capabilities", {}).get("ui"):
        return {"content": "Interactive UI requires the main Koala TUI."} if request["kind"] == "tool" else {}
    if request["kind"] == "tool":
        return {"content": "Interactive example opened. Use /extensions to interact.", "ui": [widget()]}
    event = request["event"]
    kind = event["type"]
    if kind == "mount":
        return {"ui": [widget(), {"type": "set_status", "id": "state", "text": "交互示例已就绪"}]}
    if kind == "select":
        text = {"install": "安装扩展后，把 manifest 路径加入配置并重启。", "develop": "使用 JSON 描述组件，通过 ui_event 接收用户操作。"}[event["value"]]
        return {"ui": [widget(text)]}
    if kind == "submit":
        return {"ui": [{"type": "set_widget", "id": "message", "placement": "below_editor", "blocks": [{"type": "text", "text": "收到留言：" + event["value"]}]}]}
    if kind == "click" and event["control_id"] == "reset":
        return {"ui": [widget(), {"type": "remove_widget", "id": "message"}]}
    if kind == "click":
        return {"ui": [{"type": "open_dialog", "id": "confirm", "dialog": {"kind": "confirm", "title": "确认示例", "text": "确认后显示完成提示。本示例不会修改文件。"}}]}
    if kind in ("confirm", "cancel"):
        confirmed = kind == "confirm" and event["value"]
        return {"ui": [{"type": "notify", "level": "info", "text": "已确认" if confirmed else "已取消"}, {"type": "set_status", "id": "state", "text": "确认完成" if confirmed else "操作取消"}]}
    return {}


if __name__ == "__main__":
    print(json.dumps(handle(json.load(sys.stdin)), ensure_ascii=False))
