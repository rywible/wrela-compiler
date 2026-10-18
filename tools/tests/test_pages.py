"""The docs as plain HTML pages (tools/pages.py): the export's Markdown, its anchors and links."""
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import pages


class Render(unittest.TestCase):
    def test_link_destinations_do_not_receive_text_styles(self):
        out = pages.render("[**file**](https://example.test/a**b**c?q=*value*)")
        self.assertEqual(out, '<p><a href="https://example.test/a**b**c?q=*value*"><strong>file</strong></a></p>')

    def test_fenced_code_keeps_shorter_fences_and_backticks_with_text(self):
        out = pages.render("````wrela\n\nlet a = 1\n```\n````still code\n\n````\n")
        self.assertEqual(out, "<pre><code>\nlet a = 1\n```\n````still code\n</code></pre>")

    def test_escaped_pipes_stay_in_their_table_cells(self):
        for first in ("`x\\|y`", "`x|y`"):
            out = pages.render(f"| A | B |\n|---|---|\n| {first} | *z* |\n")
            self.assertIn("<td><code>x|y</code></td><td><em>z</em></td>", out)
        self.assertEqual(pages.cells("| a || c |"), ["a", "", "c"])

    def test_link_queries_are_escaped_once(self):
        out = pages.render("[search](https://example.test/?q=fields&sort=name)")
        self.assertIn('href="https://example.test/?q=fields&amp;sort=name"', out)
        self.assertNotIn("&amp;amp;", out)

    def test_headings_take_the_exports_anchors_and_repeats_are_numbered(self):
        out = pages.render("# std::arena\n\n## Arena::insert\n\n## `Arena`\n\n## Arena\n")
        self.assertIn('<h1 id="std-arena">', out)
        self.assertIn('<h2 id="arena-insert">', out)
        self.assertIn('<h2 id="arena">', out)
        self.assertIn('<h2 id="arena-2">', out)

    def test_links_to_pages_lead_to_their_html_and_code_is_kept_as_written(self):
        out = pages.render("See [§6.8](../spec/language/6-memory.md#6-8-handles) and "
                           "[the source](https://github.com/x/a.md#L3), `a < b` and **this**.")
        self.assertIn('<a href="../spec/language/6-memory.html#6-8-handles">§6.8</a>', out)
        self.assertIn('<a href="https://github.com/x/a.md#L3">the source</a>', out)
        self.assertIn("<code>a &lt; b</code>", out)
        self.assertIn("<strong>this</strong>", out)

    def test_blocks_lists_code_tables_and_an_items_kind_under_its_heading(self):
        out = pages.render("## fetch\n\n`fn` · [`io.wrela:3`](https://x/io#L3)\n\n```wrela\nlet a = <b>\n```\n\n"
                           "- one\n  - inner\n- two\n\n3. three\n4. four\n\n| A | B |\n|---|---|\n| x | y |\n\n> quiet\n")
        self.assertIn('<p class="meta"><code>fn</code> · ', out)
        self.assertIn("<pre><code>let a = &lt;b&gt;</code></pre>", out)
        self.assertIn("<ul>\n<li>one\n<ul>\n<li>inner\n</li></ul>\n</li>\n<li>two\n</li></ul>", out)
        self.assertIn('<ol start="3">', out)
        self.assertIn("<th>A</th><th>B</th>", out)
        self.assertIn("<td>x</td><td>y</td>", out)
        self.assertIn("<blockquote><p>quiet</p></blockquote>", out)


class Write(unittest.TestCase):
    def test_each_page_is_written_beside_its_markdown_with_its_neighbours(self):
        with tempfile.TemporaryDirectory() as temporary:
            d = Path(temporary)
            (d / "std").mkdir()
            (d / "spec/language").mkdir(parents=True)
            (d / "std/io.md").write_text("# std::io\n\nRequests.\n")
            (d / "spec/language/6-memory.md").write_text("# 6. Memory\n\nValues.\n")
            (d / "index.json").write_text(json.dumps({"pages": [
                {"path": "std/io.md", "title": "std::io", "summary": "Requests.", "kind": "module"},
                {"path": "spec/language/6-memory.md", "title": "6. Memory", "summary": "Values.", "kind": "spec"},
            ]}))
            self.assertEqual(pages.write_pages(d), 2)
            memory = (d / "spec/language/6-memory.html").read_text()
            # The language first: its page's next is std's.
            self.assertIn('<a class="next" href="../../std/io.html"><span>Next</span>std::io</a>', memory)
            self.assertIn('<link rel="stylesheet" href="../../pages.css">', memory)
            self.assertIn('href="/#spec/language/6-memory">Live docs', memory)
            io = (d / "std/io.html").read_text()
            self.assertIn('<a class="previous" href="../spec/language/6-memory.html">', io)
            self.assertIn('<p class="group">Standard library</p>', io)
            self.assertTrue((d / "pages.js").is_file() and (d / "JetBrainsMono-Regular-Latin1.ttf").is_file())


if __name__ == "__main__":
    unittest.main()
