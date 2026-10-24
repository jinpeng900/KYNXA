"""Check experiment pause/resume controls without model or gateway requests.

离线核验实验的暂停、同任务续接与计量，不发送模型或网关请求。
"""
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location('api_task_harness', Path(__file__).with_name('run.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class HarnessControls(unittest.TestCase):
    def test_restart_preserves_exact_forwarder_endpoint(self):
        self.assertEqual(module.forwarder_restart_port(None), 0)
        self.assertEqual(module.forwarder_restart_port({'pressureForwarderUrl': 'http://127.0.0.1:43210'}), 43210)

    def test_restart_rejects_missing_or_nonlocal_forwarder_identity(self):
        for address in (None, 'http://example.test:43210', 'http://user@127.0.0.1:43210',
                        'http://127.0.0.1:43210/path', 'http://127.0.0.1:99999'):
            with self.subTest(address=address), self.assertRaisesRegex(module.AcceptanceError, 'RESUME_FORWARDER_IDENTITY_INVALID'):
                module.forwarder_restart_port({'pressureForwarderUrl': address})

    def bare_run(self, action='pause'):
        run = module.Run.__new__(module.Run)
        run.arguments = SimpleNamespace(guard_action=action, timeout_seconds=1800,
                                        max_calls=4, max_known_tokens=600000)
        run.started = time.monotonic()
        run.prior_usage = {}
        run.url = 'http://127.0.0.1:1'
        run.proxy = None
        run.records = [{'input': {'conversationId': 'owned-chat', 'requestId': 'owned-request'},
                        'taskId': 'owned-task', 'checks': {}}]
        run.detail = Mock(return_value={'task': {'taskId': 'owned-task', 'version': 7,
                                                'state': 'running', 'usage': {'modelRequests': 4}}})
        return run

    def test_guard_pauses_with_real_versioned_endpoint(self):
        run = self.bare_run()
        with patch.object(module, 'http_json', return_value={'task': {'taskId': 'owned-task', 'state': 'paused'}}) as request:
            with self.assertRaisesRegex(module.AcceptanceError, 'ACCEPTANCE_SHARED_COST_OR_TIME_STOP'):
                run.enforce_budget(run.records[0]['input'])
            self.assertEqual('/api/tasks/owned-task/control', request.call_args.args[2])
            self.assertEqual(request.call_args.args[3], {'conversationId': 'owned-chat', 'action': 'pause', 'expectedVersion': 7})
            self.assertTrue(run.records[0]['pauseAccepted'])

    def test_default_cancel_stays_compatible(self):
        run = self.bare_run('cancel')
        with patch.object(module, 'http_json') as request:
            with self.assertRaises(module.AcceptanceError):
                run.enforce_budget(run.records[0]['input'])
            self.assertEqual(request.call_args.args[2], '/api/chat/cancel')

    def test_resume_guard_counts_only_new_calls(self):
        run = self.bare_run()
        run.prior_usage = {'modelRequests': 4}
        with patch.object(module, 'http_json') as request:
            run.enforce_budget(run.records[0]['input'])
            request.assert_not_called()

    def test_settled_result_is_not_cancelled_by_late_guard(self):
        run = self.bare_run()
        run.detail.return_value['task']['state'] = 'completed'
        with patch.object(module, 'http_json') as request:
            run.enforce_budget(run.records[0]['input'])
            request.assert_not_called()
            self.assertTrue(run.records[0]['guardReachedAfterSettlement'])

    def test_cancelled_task_is_rejected_before_reading_model_profile(self):
        with tempfile.TemporaryDirectory(prefix='kynxa-api-tasks-') as directory:
            root = Path(directory)
            module.write_json(root / '.api-task-smoke-owner.json', {'ownerId': 'offline-owner'})
            report = root / 'paused-report.json'
            module.write_json(report, {'isolatedRoot': str(root), 'isolationOwnerId': 'offline-owner',
                                       'scenarios': [{'detail': {'task': {'state': 'cancelled'}}}]})
            with self.assertRaisesRegex(module.AcceptanceError, 'RESUME_REQUIRES_FORMALLY_PAUSED_SAME_TASK'):
                module.Run(SimpleNamespace(resume_existing=report))

    def test_close_preserves_formal_pause_and_removes_credentials(self):
        run = self.bare_run()
        with tempfile.TemporaryDirectory() as directory:
            run.root = Path(directory)
            run.profile = run.root / 'connections.json'
            run.profile.write_text('{}')
            run.arguments.result_path = run.root / 'result.json'
            run.records[0]['budgetGuard'] = {'action': 'pause'}
            run.detail.return_value = {'task': {'state': 'paused', 'usage': {'modelRequests': 4}},
                                       'attempt': {'endedAtMs': 1}, 'children': []}
            run.process = Mock()
            run.process.poll.side_effect = [None, None]
            run.stdout = run.stderr = None
            run.connection = {'apiKey': 'simulated-only'}
            run.resume_record = None
            run.previous_report = None
            run.report = {'scenarios': run.records}
            with patch.object(module, 'http_json') as request, patch.object(module.subprocess, 'run'):
                run.close()
                request.assert_not_called()
            self.assertFalse(run.profile.exists())
            self.assertTrue(run.records[0]['pauseSettled'])
            self.assertTrue(run.records[0]['pausedDataRetained'])
            self.assertEqual(run.records[0]['detail']['task']['state'], 'paused')

    def test_resume_uses_same_id_control_and_get_stream(self):
        run = self.bare_run()
        run.arguments.stop_on_summary_rejection = False
        input_value = dict(run.records[0]['input'], runLimits={'maxRounds': 32, 'maxToolCalls': 80})
        run.records = []
        run.new_chat = Mock(side_effect=AssertionError('replacement chat forbidden'))
        task = {'taskId': 'owned-task', 'requestId': 'owned-request', 'version': 7, 'state': 'paused',
                'scope': {'conversationId': 'owned-chat'}, 'goal': 'preserved original goal',
                'inputReference': {'kind': 'task-input', 'sha256': 'frozen-original-input'}}
        run.resume_record = {'taskId': 'owned-task', 'input': input_value, 'detail': {'task': dict(task)}}
        run.detail.side_effect = [{'task': task}, {'task': dict(task, state='completed'), 'children': []}]
        events = [{'type': 'started'}, {'type': 'task_started', 'taskId': 'owned-task', 'rootRequestTaskId': 'owned-task'},
                  {'type': 'completed', 'content': 'complete'}]
        response = io.BytesIO(b''.join(b'data: ' + json.dumps(event).encode() + b'\n\n' for event in events))
        final = {'kind': 'task.final', 'sequence': 1,
                 'payload': {'completion': {'mutationRevision': 1,
                                           'receipts': [{'exitCode': 0, 'coversObservedRevision': True,
                                                         'mutationRevision': 1}]}}}

        def gateway(_base, method, path, body=None, timeout=30):
            if method == 'POST' and path.endswith('/control'):
                self.assertEqual(body, {'conversationId': 'owned-chat', 'action': 'resume', 'expectedVersion': 7})
                return {'task': dict(task, state='queued')}
            self.assertEqual(method, 'GET')
            self.assertIn('/events?', path)
            return {'events': [final], 'hasMore': False, 'throughSequence': 1}

        with patch.object(module, 'http_json', side_effect=gateway), patch.object(module.LOCAL_HTTP, 'open', return_value=response) as stream:
            record = run.perform('same-task-resume', 'ignored replacement prompt')
            request = stream.call_args.args[0]
            self.assertEqual(request.method, 'GET')
            self.assertIn('/api/tasks/owned-task/stream?', request.full_url)
            self.assertTrue(record['sameTaskResumed'])
            self.assertEqual(record['input']['runLimits'], input_value['runLimits'])
            run.new_chat.assert_not_called()

    def test_frozen_fixture_mismatch_is_rejected(self):
        run = self.bare_run()
        with tempfile.TemporaryDirectory() as directory:
            run.work = Path(directory)
            run.report = {}
            run.previous_report = {'longFixture': {'sha256': {}, 'expected': {}}}
            with self.assertRaisesRegex(module.AcceptanceError, 'RESUME_FROZEN_FIXTURE_CHANGED'):
                run.prepare_long_fixture()


if __name__ == '__main__':
    unittest.main(verbosity=2)
