#!/bin/bash
# 新增检查（第 16 条）的正 / 负向验证。
#
# 负向要**分别**验两条：拿掉出错页的底栏、拿掉出错页的退出入口。
set -u
cd "$(dirname "$0")/.." || exit 1
PROJ=$(cd "$(dirname "$0")/.." && pwd)

run_check() {
    bash "$PROJ/check-ui-spec.sh" 2>&1 | grep -A9 '16\. 每一屏'
    bash "$PROJ/check-ui-spec.sh" > /dev/null 2>&1
    echo "   整体退出码 = $?"
}

echo "########## 正向：现在应该全 OK ##########"
run_check

echo
echo "########## 负向 A：拿掉出错页的退出按钮 ##########"
cp ui/app.slint /tmp/app.bak
python3 - <<'PY'
p = "ui/app.slint"
s = open(p, encoding="utf-8").read()
# 只改第 9 屏里那一处调用（它在退出按钮的 TouchArea 里）
old = """            TouchArea {
                clicked => { root.quit-requested(); }
            }
        }

        Text {
            x: 60px;
            y: 88px + 70px;
            // 右边让开右上角那个退出按钮"""
new = """            TouchArea {
                clicked => { root.retry(); }
            }
        }

        Text {
            x: 60px;
            y: 88px + 70px;
            // 右边让开右上角那个退出按钮"""
if old not in s:
    print("！！没找到片段，负向测试无效"); raise SystemExit(1)
open(p, "w", encoding="utf-8").write(s.replace(old, new, 1))
print("   已把出错页的 quit-requested 改成 retry")
PY
run_check

echo
echo "########## 负向 B：拿掉出错页的底栏 ##########"
cp /tmp/app.bak ui/app.slint
python3 - <<'PY'
p = "ui/app.slint"
s = open(p, encoding="utf-8").read()
# 第 9 屏末尾那个 NavBar（它前面是 loading 面板的收尾）
old = """        NavBar {
            y: 1648px - 108px;
            active: root.nav-active;
            picked(i) => { root.nav-picked(i); }
        }
    }

    // ================================================================
    // 数字小键盘覆盖层（规格 4.6）"""
new = """    }

    // ================================================================
    // 数字小键盘覆盖层（规格 4.6）"""
if old not in s:
    print("！！没找到片段，负向测试无效"); raise SystemExit(1)
open(p, "w", encoding="utf-8").write(s.replace(old, new, 1))
print("   已拿掉出错页的 NavBar")
PY
run_check

echo
echo "########## 还原 ##########"
cp /tmp/app.bak ui/app.slint
grep -c 'NavBar {' ui/app.slint
bash "$PROJ/check-ui-spec.sh" > /dev/null 2>&1; echo "还原后退出码 = $?（应为 0）"
