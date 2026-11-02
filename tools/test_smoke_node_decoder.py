import json
import unittest
from smoke_node_decoder import PACKAGE, validate
from android_session import SessionError

class DecoderSmokeTests(unittest.TestCase):
    def session(self):
        return {"exit_code":0,"cleanup_removed":True,"stdout":json.dumps({
            "hierarchy_available":False,
            "outcome":{"initialized":True,"unregistered":True,"error":None,"cleanup_error":None,"query":{"fields_decoded":True}},
            "node":{"package":PACKAGE,"text":"CARLO_STATE_0","content_description":"Carlo ação 🪵 <&>","resource_id":PACKAGE+":id/fixture_root","unique_id":"carlo-fixture-root","window_id":1,"source_id":23,"class":"View","properties":{"enabled":True,"clickable":True,"password":False},"bounds_in_screen":{"left":0,"top":0,"right":720,"bottom":1280}}
        })}
    def test_requires_decoded_expected_fields_and_cleanup(self):
        self.assertEqual(validate(self.session(),"CARLO_STATE_0")["right"],720)
        session=self.session();session["cleanup_removed"]=False
        with self.assertRaises(SessionError): validate(session,"CARLO_STATE_0")
        session=self.session();report=json.loads(session['stdout']);report['outcome']['query']['fields_decoded']=False;session['stdout']=json.dumps(report)
        with self.assertRaises(SessionError): validate(session,"CARLO_STATE_0")
    def test_mismatch_error_never_includes_ui_contents(self):
        session=self.session();report=json.loads(session['stdout']);report['node']['text']='SENSITIVE_TEST_SENTINEL';session['stdout']=json.dumps(report)
        with self.assertRaises(SessionError) as error: validate(session,"CARLO_STATE_0")
        self.assertNotIn('SENSITIVE_TEST_SENTINEL',str(error.exception))
