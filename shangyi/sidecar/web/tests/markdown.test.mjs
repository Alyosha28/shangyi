import test from 'node:test';
import assert from 'node:assert/strict';
import {renderMarkdown} from '../js/markdown.js';

// An excerpt from the real synthetic budget consultation, not fabricated financial data.
const MODEL_EXCERPT=`## 先复述我的理解

这是一个**虚构**的小店预算测试，因此**不做任何外部检索**。

按这个口径，我只做了两件确定性动作：\`business_cost\`与\`vn_map\`。

---

## 一、已取得的支出项

| 项目 | 金额 | 性质 | 时点 |
|---|---|---|---|
| 装修工程款 | ¥80,000 | 现金费用（实际支付） | 一次性 |
| 设备采购款 | ¥30,000 | 现金费用（实际支付） | 一次性 |
| 月租金 | ¥6,000 | 现金费用（实际支付） | 每月 |
| **已列一次性现金支出合计** | **¥110,000** | | |

说明两点口径：
- 8万装修、3万设备是**支付时的现金流出**；折旧不重复计。
- 6,000元是**每月**的经常性支出。

## 二、暂时未被已列支出占用的现金

**¥200,000 − ¥110,000 = ¥90,000**

由此，\`costs_complete = false\`。本轮答案是：**无法判断**。`;

test('real model headings, cost table, bullets, emphasis and code become readable blocks',()=>{
 const html=renderMarkdown(MODEL_EXCERPT);
 assert.match(html,/<h2>先复述我的理解<\/h2>/);
 assert.equal((html.match(/<table>/g)||[]).length,1);
 assert.equal((html.match(/<th scope="col"/g)||[]).length,4);
 assert.equal((html.match(/<td /g)||[]).length,16);
 assert.match(html,/<strong>¥110,000<\/strong>/);
 assert.match(html,/<ul><li><p>8万装修/);
 assert.match(html,/<code>business_cost<\/code>/);
 assert.match(html,/<strong>无法判断<\/strong>/);
 assert.ok(!html.includes('|---|'));
 assert.ok(!html.includes('## 先复述'));
 assert.match(html,/role="region" aria-label="表格，可横向滚动" tabindex="0"/);
});

test('raw HTML and HTML-looking labels always remain escaped text',()=>{
 const html=renderMarkdown('## <img src=x onerror=alert(1)>\n\n<script>alert(1)</script>\n\n[<svg onload=alert(1)>](https://example.com)');
 assert.ok(!/<(?:script|img|svg)\b/i.test(html));
 assert.match(html,/&lt;script&gt;alert\(1\)&lt;\/script&gt;/);
 assert.match(html,/&lt;img src=x onerror=alert\(1\)&gt;/);
 assert.match(html,/<a href="https:\/\/example.com\/" target="_blank" rel="noopener noreferrer">&lt;svg/);
 assert.ok(!/<[^>]+\s(?:onload|onerror|onclick)=/i.test(html));
});

test('only safe HTTP(S) destinations become links, with attribute escaping',()=>{
 const html=renderMarkdown([
  '[正常](https://example.com/a_(b)?a=1&b=2)',
  '[HTTP](http://example.com)',
  '[执行](javascript:alert(1))',
  '[数据](data:text/html,<script>alert(1)</script>)',
  '[文件](file:///etc/passwd)',
  '[相对](//evil.example)',
  '[换行](java\nscript:alert(1))',
  '[注入](https://example.com/" onclick="alert(1))',
  '[实体](&#106;avascript:alert(1))'
 ].join('\n\n'));
 assert.equal((html.match(/<a /g)||[]).length,2);
 assert.match(html,/href="https:\/\/example.com\/a_\(b\)\?a=1&amp;b=2"/);
 assert.ok(!/<[^>]+\sonclick=/i.test(html));
 assert.match(html,/\[执行\]\(javascript:alert\(1\)\)/);
});

test('bare source URLs become clickable without swallowing punctuation or linking credentials and code',()=>{
 const html=renderMarkdown('官方出处：https://www.shanghai.gov.cn/gwk/search/content/abc。\n\n(https://example.com/a_(b)?x=1&y=2).\n\nhttps://user:secret@example.com/page\n\n`https://example.com/code`\n\n[https://example.com/label](https://example.com/target)');
 assert.equal((html.match(/<a /g)||[]).length,3);
 assert.match(html,/href="https:\/\/www.shanghai.gov.cn\/gwk\/search\/content\/abc"/);
 assert.match(html,/href="https:\/\/example.com\/a_\(b\)\?x=1&amp;y=2"/);
 assert.match(html,/code>https:\/\/example.com\/code<\/code/);
 assert.ok(!html.includes('href="https://user:secret'));
 assert.match(html,/target="_blank" rel="noopener noreferrer"/);
});

test('image syntax never creates an image or an automatic request',()=>{
 const html=renderMarkdown('![外部追踪图](https://example.com/pixel.png)\n\n![恶意](data:image/svg+xml,<svg onload=alert(1)>)\n\n<img src="https://example.com/tracker">');
 assert.ok(!/<(?:img|iframe|object|embed|source|video|audio|link)\b/i.test(html));
 assert.ok(!/<a /i.test(html));
 assert.match(html,/!\[外部追踪图\]\(https:\/\/example.com\/pixel.png\)/);
});

test('code blocks and inline code escape markup and do not interpret Markdown',()=>{
 const html=renderMarkdown('```html " onmouseover="bad\n<script>danger()</script>\n**只是代码**\n```\n\n`<img src=x>` 与 **真正加粗**');
 assert.match(html,/<pre><code>&lt;script&gt;danger\(\)&lt;\/script&gt;\n\*\*只是代码\*\*<\/code><\/pre>/);
 assert.match(html,/<code>&lt;img src=x&gt;<\/code>/);
 assert.match(html,/<strong>真正加粗<\/strong>/);
 assert.ok(!/<[^>]+\sonmouseover=/i.test(html));
});

test('tables preserve escaped pipes, code pipes and incomplete rows without dropping text',()=>{
 const html=renderMarkdown('| 条件 | 结果 |\n|:---|---:|\n| A\\|B | `x|y` |\n| 不完整的一行 |');
 assert.match(html,/class="md-align-left">A\|B<\/td>/);
 assert.match(html,/class="md-align-right"><code>x\|y<\/code><\/td>/);
 assert.match(html,/<p>\| 不完整的一行 \|<\/p>/);
});

test('lists retain numbering and nesting while unsupported or unmatched syntax stays readable',()=>{
 const html=renderMarkdown('3. 第三项\n   - 子项目\n4. 第四项\n\n## C#\n\n成本 snake_case 不应变成斜体。\n\n**未结束的加粗\n\n> 暂不转换的引用');
 assert.match(html,/<ol start="3">/);
 assert.match(html,/<ul><li><p>子项目<\/p><\/li><\/ul>/);
 assert.match(html,/<h2>C#<\/h2>/);
 assert.match(html,/snake_case/);
 assert.match(html,/\*\*未结束的加粗/);
 assert.match(html,/&gt; 暂不转换的引用/);
});
