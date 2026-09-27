import json
import threading
import unittest
from http.server import HTTPServer
from urllib.request import Request, urlopen
from urllib.error import HTTPError

from server import Handler


class ServerTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = HTTPServer(("127.0.0.1", 0), Handler)
        cls.base = f"http://127.0.0.1:{cls.server.server_port}"
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()

    def test_hello(self):
        with urlopen(self.base + "/hello") as response:
            self.assertEqual(response.status, 200)
            self.assertIn(b"hello", response.read())

    def test_unknown_path_is_404(self):
        with self.assertRaises(HTTPError) as caught:
            urlopen(self.base + "/nope")
        self.assertEqual(caught.exception.code, 404)

    def test_post_then_list_notes(self):
        request = Request(self.base + "/notes", data=json.dumps({"text": "t"}).encode(), method="POST")
        with urlopen(request) as response:
            self.assertEqual(response.status, 201)
        with urlopen(self.base + "/notes") as response:
            self.assertIn("t", [note["text"] for note in json.load(response)])


if __name__ == "__main__":
    unittest.main()
