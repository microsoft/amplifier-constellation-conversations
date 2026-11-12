"""Guard the CI controller's isolation/cleanup contract; not native qualification."""
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

spec=importlib.util.spec_from_file_location('clean_userland',Path(__file__).with_name('native_clean_userland.py'))
native=importlib.util.module_from_spec(spec); spec.loader.exec_module(native)


class CleanUserlandContract(unittest.TestCase):
    def test_both_native_targets_keep_isolation(self):
        for target,architecture in [('linux-x64','linux/amd64'),('linux-arm64','linux/arm64')]:
            args=native.command('owned','run',target,Path('/a Unicode 日本語'),Path('/fixture'),['/bin/true'])
            for flag,value in [('--platform',architecture),('--pull','never'),('--network','none'),
                    ('--cap-drop','ALL'),('--security-opt','no-new-privileges'),('--user','1000:1000')]:
                self.assertEqual(args[args.index(flag)+1],value)
            self.assertIn('--read-only',args)
            self.assertIn('--rm',args)
            self.assertIn(native.IMAGE,args)
            mounts=[args[i+1] for i,x in enumerate(args) if x=='--mount']
            self.assertTrue(all(value.endswith(',readonly') for value in mounts))
            self.assertIn('/home/fixture:rw,exec,nosuid,nodev,uid=1000,gid=1000,mode=0700,size=1024m',args)

    def test_non_linux_target_refused(self):
        with self.assertRaisesRegex(ValueError,'only supports native Linux'):
            native.command('owned','run','windows-arm64',Path('/a'),Path('/b'),[])

    def test_cleanup_never_removes_another_owner(self):
        with patch.object(native,'capture',return_value={'returncode':0,'stdout':'{"io.constellation.native-userland":"other"}'}) as call:
            with self.assertRaisesRegex(ValueError,'owner changed'):
                native.cleanup('owned','run')
            self.assertEqual(call.call_count,1)

    def test_cleanup_only_removes_matching_owned_container(self):
        with patch.object(native,'capture',side_effect=[{'returncode':0,'stdout':'{"io.constellation.native-userland":"run"}'},
                {'returncode':0,'stdout':''}]) as call:
            self.assertTrue(native.cleanup('owned','run')['removed'])
            self.assertEqual(call.call_args.args[0],['docker','rm','-f','owned'])

    def test_absent_container_needs_no_mutation(self):
        with patch.object(native,'capture',return_value={'returncode':1,'stdout':'',
                'stderr':'error: no such object: owned'}) as call:
            self.assertTrue(native.cleanup('owned','run')['absent'])
            self.assertEqual(call.call_count,1)

    def test_unavailable_docker_is_not_proof_of_cleanup(self):
        with patch.object(native,'capture',return_value={'returncode':1,'stdout':'',
                'stderr':'Cannot connect to the Docker daemon'}) as call:
            with self.assertRaisesRegex(ValueError,'Cannot prove container absence'):
                native.cleanup('owned','run')
            self.assertEqual(call.call_count,1)


if __name__=='__main__':
    unittest.main()
