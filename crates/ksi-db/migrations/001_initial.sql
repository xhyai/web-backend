
CREATE TABLE cache (
	"key" VARCHAR(220) NOT NULL, 
	value TEXT NOT NULL, 
	expires TIMESTAMP NOT NULL, 
	PRIMARY KEY ("key")
)

;


CREATE TABLE config (
	"key" VARCHAR(100) NOT NULL, 
	value TEXT, 
	secret BOOLEAN NOT NULL, 
	PRIMARY KEY ("key")
)

;


CREATE TABLE feedback_recipients (
	email VARCHAR(150) NOT NULL, 
	PRIMARY KEY (email)
)

;


CREATE TABLE mail_eastereggs (
	id INTEGER NOT NULL, 
	body TEXT NOT NULL, 
	PRIMARY KEY (id)
)

;


CREATE TABLE prerequisities (
	id INTEGER NOT NULL, 
	type VARCHAR(6) DEFAULT 'ATOMIC' NOT NULL, 
	parent INTEGER, 
	task INTEGER, 
	PRIMARY KEY (id), 
	FOREIGN KEY(parent) REFERENCES prerequisities (id) ON DELETE CASCADE, 
	FOREIGN KEY(task) REFERENCES tasks (id)
)

;


CREATE TABLE tasks (
	id INTEGER NOT NULL, 
	title VARCHAR(255) NOT NULL, 
	author INTEGER, 
	co_author INTEGER, 
	wave INTEGER NOT NULL, 
	prerequisite INTEGER, 
	intro VARCHAR(500) NOT NULL, 
	body TEXT NOT NULL, 
	solution TEXT, 
	thread INTEGER NOT NULL, 
	picture_base VARCHAR(255), 
	time_created DATETIME, 
	time_deadline DATETIME, 
	evaluation_public BOOLEAN NOT NULL, 
	git_path VARCHAR(255), 
	git_branch VARCHAR(255), 
	git_commit VARCHAR(255), 
	git_pull_id INTEGER, 
	deploy_date DATETIME, 
	deploy_status VARCHAR(9) NOT NULL, 
	eval_comment TEXT, 
	PRIMARY KEY (id), 
	FOREIGN KEY(author) REFERENCES users (id), 
	FOREIGN KEY(co_author) REFERENCES users (id), 
	FOREIGN KEY(wave) REFERENCES waves (id), 
	FOREIGN KEY(prerequisite) REFERENCES prerequisities (id) ON DELETE SET NULL, 
	FOREIGN KEY(thread) REFERENCES threads (id)
)

;


CREATE TABLE users (
	id INTEGER NOT NULL, 
	email VARCHAR(50) NOT NULL, 
	github VARCHAR(50), 
	discord VARCHAR(50), 
	phone VARCHAR(15), 
	first_name VARCHAR(50) NOT NULL, 
	nick_name VARCHAR(50), 
	last_name VARCHAR(50) NOT NULL, 
	sex VARCHAR(6) NOT NULL, 
	password VARCHAR(255) NOT NULL, 
	short_info TEXT NOT NULL, 
	profile_picture VARCHAR(255), 
	role VARCHAR(18) DEFAULT 'participant' NOT NULL, 
	enabled BOOLEAN DEFAULT '1' NOT NULL, 
	registered TIMESTAMP DEFAULT CURRENT_TIMESTAMP NOT NULL, 
	last_logged_in TIMESTAMP, 
	PRIMARY KEY (id), 
	UNIQUE (email)
)

;


CREATE TABLE years (
	id INTEGER NOT NULL, 
	year VARCHAR(100), 
	sealed BOOLEAN NOT NULL, 
	point_pad DECIMAL(10, 1) NOT NULL, 
	PRIMARY KEY (id)
)

;


CREATE TABLE achievements (
	id INTEGER NOT NULL, 
	title VARCHAR(255) NOT NULL, 
	picture VARCHAR(128) NOT NULL, 
	description VARCHAR(200), 
	year INTEGER, 
	PRIMARY KEY (id), 
	FOREIGN KEY(year) REFERENCES years (id)
)

;


CREATE TABLE active_orgs (
	org INTEGER NOT NULL, 
	year INTEGER NOT NULL, 
	PRIMARY KEY (org, year), 
	FOREIGN KEY(org) REFERENCES users (id) ON DELETE CASCADE, 
	FOREIGN KEY(year) REFERENCES years (id) ON DELETE CASCADE
)

;


CREATE TABLE articles (
	id INTEGER NOT NULL, 
	author INTEGER NOT NULL, 
	title VARCHAR(255) NOT NULL, 
	body TEXT, 
	picture VARCHAR(255), 
	time_created TIMESTAMP DEFAULT CURRENT_TIMESTAMP, 
	published BOOLEAN, 
	year INTEGER NOT NULL, 
	resource VARCHAR(512), 
	PRIMARY KEY (id), 
	FOREIGN KEY(author) REFERENCES users (id), 
	FOREIGN KEY(year) REFERENCES years (id)
)

;


CREATE TABLE audit_logs (
	id INTEGER NOT NULL, 
	created TIMESTAMP DEFAULT CURRENT_TIMESTAMP NOT NULL, 
	user_id INTEGER, 
	year_id INTEGER, 
	scope VARCHAR(50), 
	line TEXT NOT NULL, 
	line_meta TEXT, 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES users (id) ON DELETE CASCADE, 
	FOREIGN KEY(year_id) REFERENCES years (id) ON DELETE CASCADE
)

;


CREATE TABLE diplomas (
	user_id INTEGER NOT NULL, 
	year_id INTEGER NOT NULL, 
	revoked BOOLEAN NOT NULL, 
	PRIMARY KEY (user_id, year_id), 
	FOREIGN KEY(user_id) REFERENCES users (id) ON DELETE CASCADE, 
	FOREIGN KEY(year_id) REFERENCES years (id) ON DELETE CASCADE
)

;


CREATE TABLE feedbacks (
	user INTEGER NOT NULL, 
	task INTEGER NOT NULL, 
	content TEXT NOT NULL, 
	"lastUpdated" TIMESTAMP DEFAULT CURRENT_TIMESTAMP, 
	PRIMARY KEY (user, task), 
	FOREIGN KEY(user) REFERENCES users (id) ON DELETE CASCADE ON UPDATE NO ACTION, 
	FOREIGN KEY(task) REFERENCES tasks (id) ON DELETE CASCADE ON UPDATE NO ACTION
)

;


CREATE TABLE modules (
	id INTEGER NOT NULL, 
	task INTEGER NOT NULL, 
	type VARCHAR(11) NOT NULL, 
	name VARCHAR(255) NOT NULL, 
	description TEXT, 
	max_points DECIMAL(10, 1) NOT NULL, 
	autocorrect BOOLEAN DEFAULT FALSE NOT NULL, 
	"order" SMALLINT DEFAULT '1' NOT NULL, 
	bonus BOOLEAN DEFAULT FALSE NOT NULL, 
	custom BOOLEAN NOT NULL, 
	action TEXT, 
	data TEXT, 
	submit_ratelimit INTEGER, 
	PRIMARY KEY (id), 
	FOREIGN KEY(task) REFERENCES tasks (id) ON DELETE CASCADE
)

;


CREATE TABLE oauth2_tokens (
	access_token VARCHAR(150) NOT NULL, 
	user INTEGER, 
	expire DATETIME, 
	refresh_token VARCHAR(150), 
	granted DATETIME, 
	PRIMARY KEY (access_token), 
	FOREIGN KEY(user) REFERENCES users (id)
)

;


CREATE TABLE profiles (
	user_id INTEGER NOT NULL, 
	addr_street VARCHAR(255) NOT NULL, 
	addr_city VARCHAR(255) NOT NULL, 
	addr_zip VARCHAR(20) NOT NULL, 
	addr_country VARCHAR(255) NOT NULL, 
	school_name VARCHAR(255) NOT NULL, 
	school_street VARCHAR(255) NOT NULL, 
	school_city VARCHAR(255) NOT NULL, 
	school_zip VARCHAR(20) NOT NULL, 
	school_country VARCHAR(255) NOT NULL, 
	school_finish INTEGER NOT NULL, 
	tshirt_size VARCHAR(2) NOT NULL, 
	referral VARCHAR(4096), 
	PRIMARY KEY (user_id), 
	FOREIGN KEY(user_id) REFERENCES users (id) ON DELETE CASCADE
)

;


CREATE TABLE threads (
	id INTEGER NOT NULL, 
	title VARCHAR(1000), 
	public BOOLEAN DEFAULT TRUE NOT NULL, 
	year INTEGER NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(year) REFERENCES years (id)
)

;


CREATE TABLE users_notify (
	user INTEGER NOT NULL, 
	auth_token VARCHAR(255) NOT NULL, 
	notify_eval BOOLEAN NOT NULL, 
	notify_response BOOLEAN NOT NULL, 
	notify_ksi BOOLEAN NOT NULL, 
	notify_events BOOLEAN NOT NULL, 
	PRIMARY KEY (user), 
	FOREIGN KEY(user) REFERENCES users (id) ON DELETE CASCADE
)

;


CREATE TABLE waves (
	id INTEGER NOT NULL, 
	year INTEGER NOT NULL, 
	"index" INTEGER NOT NULL, 
	caption VARCHAR(100), 
	garant INTEGER NOT NULL, 
	time_published DATETIME NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(year) REFERENCES years (id), 
	FOREIGN KEY(garant) REFERENCES users (id)
)

;


CREATE TABLE code_executions (
	id INTEGER NOT NULL, 
	module INTEGER NOT NULL, 
	user INTEGER NOT NULL, 
	code TEXT, 
	result VARCHAR(5), 
	time TIMESTAMP DEFAULT CURRENT_TIMESTAMP, 
	report TEXT, 
	PRIMARY KEY (id), 
	FOREIGN KEY(module) REFERENCES modules (id) ON DELETE CASCADE, 
	FOREIGN KEY(user) REFERENCES users (id) ON DELETE CASCADE
)

;


CREATE TABLE evaluations (
	id INTEGER NOT NULL, 
	user INTEGER NOT NULL, 
	module INTEGER NOT NULL, 
	evaluator INTEGER, 
	points DECIMAL(10, 1) NOT NULL, 
	ok BOOLEAN DEFAULT FALSE NOT NULL, 
	cheat BOOLEAN NOT NULL, 
	full_report TEXT NOT NULL, 
	time TIMESTAMP DEFAULT CURRENT_TIMESTAMP, 
	PRIMARY KEY (id), 
	FOREIGN KEY(user) REFERENCES users (id) ON DELETE CASCADE, 
	FOREIGN KEY(module) REFERENCES modules (id), 
	FOREIGN KEY(evaluator) REFERENCES users (id)
)

;


CREATE TABLE modules_custom (
	module INTEGER NOT NULL, 
	user INTEGER NOT NULL, 
	description TEXT, 
	description_replace TEXT, 
	data TEXT, 
	error TEXT, 
	PRIMARY KEY (module, user), 
	FOREIGN KEY(module) REFERENCES modules (id) ON DELETE CASCADE ON UPDATE NO ACTION, 
	FOREIGN KEY(user) REFERENCES users (id) ON DELETE CASCADE ON UPDATE NO ACTION
)

;


CREATE TABLE posts (
	id INTEGER NOT NULL, 
	thread INTEGER NOT NULL, 
	author INTEGER NOT NULL, 
	body TEXT NOT NULL, 
	published_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP NOT NULL, 
	parent INTEGER, 
	resolved_at TIMESTAMP, 
	PRIMARY KEY (id), 
	FOREIGN KEY(thread) REFERENCES threads (id) ON DELETE CASCADE, 
	FOREIGN KEY(author) REFERENCES users (id), 
	FOREIGN KEY(parent) REFERENCES posts (id) ON DELETE CASCADE
)

;


CREATE TABLE solution_comments (
	thread INTEGER NOT NULL, 
	user INTEGER NOT NULL, 
	task INTEGER NOT NULL, 
	PRIMARY KEY (thread, user, task), 
	FOREIGN KEY(thread) REFERENCES threads (id), 
	FOREIGN KEY(user) REFERENCES users (id), 
	FOREIGN KEY(task) REFERENCES tasks (id)
)

;


CREATE TABLE text (
	id INTEGER NOT NULL, 
	module INTEGER NOT NULL, 
	inputs INTEGER, 
	diff TEXT, 
	ignore_case BOOLEAN, 
	eval_script VARCHAR(255), 
	PRIMARY KEY (id), 
	FOREIGN KEY(module) REFERENCES modules (id)
)

;


CREATE TABLE threads_visits (
	thread INTEGER NOT NULL, 
	user INTEGER NOT NULL, 
	last_visit TIMESTAMP DEFAULT CURRENT_TIMESTAMP NOT NULL, 
	last_last_visit TIMESTAMP, 
	PRIMARY KEY (thread, user), 
	FOREIGN KEY(thread) REFERENCES threads (id) ON DELETE CASCADE, 
	FOREIGN KEY(user) REFERENCES users (id) ON DELETE CASCADE
)

;


CREATE TABLE user_achievement (
	user_id INTEGER NOT NULL, 
	achievement_id INTEGER NOT NULL, 
	task_id INTEGER, 
	PRIMARY KEY (user_id, achievement_id), 
	FOREIGN KEY(user_id) REFERENCES users (id) ON DELETE CASCADE, 
	FOREIGN KEY(achievement_id) REFERENCES achievements (id) ON DELETE CASCADE, 
	FOREIGN KEY(task_id) REFERENCES tasks (id) ON DELETE CASCADE
)

;


CREATE TABLE submitted_codes (
	id INTEGER NOT NULL, 
	evaluation INTEGER NOT NULL, 
	code TEXT NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(evaluation) REFERENCES evaluations (id) ON DELETE CASCADE
)

;


CREATE TABLE submitted_files (
	id INTEGER NOT NULL, 
	evaluation INTEGER NOT NULL, 
	mime VARCHAR(255), 
	path VARCHAR(255) NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(evaluation) REFERENCES evaluations (id) ON DELETE CASCADE
)

;

